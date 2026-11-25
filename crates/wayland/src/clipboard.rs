//! The clipboard over wlr-data-control, on its own connection and
//! `vivus-clipboard` thread. We bind v1, so the primary selection
//! (middle-click paste) never reaches us.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::pipe::{PipeFlags, pipe_with};
use vivus_core::{ClipboardBackend, ClipboardContent, ClipboardError};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self as device, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::{self as manager, ZwlrDataControlManagerV1},
    zwlr_data_control_offer_v1::{self as offer, ZwlrDataControlOfferV1},
    zwlr_data_control_source_v1::{self as source, ZwlrDataControlSourceV1},
};

/// KeePassXC, Bitwarden and KDE's own tools mark passwords with this type.
const PASSWORD_HINT: &str = "x-kde-passwordManagerHint";
/// In order of preference; the last two are X11 names that XWayland apps offer.
const TEXT_TYPES: &[&str] = &[
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
    "TEXT",
];
const READ_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_BYTES: usize = 1 << 20;

/// MIME types an offer has announced so far.
type Types = Mutex<Vec<String>>;

pub struct Clipboard {
    conn: Connection,
    qh: QueueHandle<State>,
    manager: ZwlrDataControlManagerV1,
    device: ZwlrDataControlDeviceV1,
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    selection: Option<ZwlrDataControlOfferV1>,
    /// Text we put on the clipboard, while it's still ours.
    owned: Option<(ZwlrDataControlSourceV1, Arc<str>)>,
    /// The device stopped working (seat gone, connection lost).
    finished: bool,
}

impl Clipboard {
    /// Connects and reads the current selection before returning.
    pub fn connect() -> anyhow::Result<Self> {
        let conn = Connection::connect_to_env().context("no Wayland display")?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let manager = globals
            .bind::<ZwlrDataControlManagerV1, _, _>(&qh, 1..=1, ())
            .context("compositor has no wlr-data-control")?;
        let seat = globals
            .bind::<WlSeat, _, _>(&qh, 1..=1, ())
            .context("compositor has no seat")?;
        let device = manager.get_data_device(&seat, &qh, ());
        let inner = Arc::new(Mutex::new(Inner::default()));
        let mut state = State {
            inner: inner.clone(),
        };
        queue.roundtrip(&mut state)?;
        tracing::info!("clipboard: wlr-data-control");
        std::thread::Builder::new()
            .name("vivus-clipboard".into())
            .spawn(move || {
                loop {
                    if let Err(e) = queue.blocking_dispatch(&mut state) {
                        tracing::error!("clipboard: Wayland connection lost: {e}");
                        state.inner.lock().unwrap().finished = true;
                        return;
                    }
                }
            })
            .context("spawning clipboard thread")?;
        Ok(Self {
            conn,
            qh,
            manager,
            device,
            inner,
        })
    }
}

impl ClipboardBackend for Clipboard {
    fn read(&self) -> Result<ClipboardContent, ClipboardError> {
        let offer = {
            let inner = self.inner.lock().unwrap();
            if inner.finished {
                return Err(ClipboardError::Backend(
                    "the clipboard connection closed".into(),
                ));
            }
            if let Some((_, text)) = &inner.owned {
                return Ok(ClipboardContent::Text(text.to_string()));
            }
            match &inner.selection {
                Some(o) => o.clone(),
                None => return Ok(ClipboardContent::Empty),
            }
        };
        let types = offer
            .data::<Types>()
            .map(|t| t.lock().unwrap().clone())
            .unwrap_or_default();
        if types.iter().any(|t| t == PASSWORD_HINT) {
            return Ok(ClipboardContent::Secret);
        }
        let Some(mime) = TEXT_TYPES.iter().find(|m| types.iter().any(|t| t == *m)) else {
            return Ok(if types.is_empty() {
                ClipboardContent::Empty
            } else {
                ClipboardContent::Other(types)
            });
        };
        let backend = |e: std::io::Error| ClipboardError::Backend(e.to_string());
        let (r, w) = pipe_with(PipeFlags::CLOEXEC).map_err(|e| backend(e.into()))?;
        offer.receive((*mime).into(), w.as_fd());
        self.conn
            .flush()
            .map_err(|e| ClipboardError::Backend(e.to_string()))?;
        // The sender's copy is all that's left, so EOF comes when it's done.
        drop(w);
        let bytes = read_pipe(r)?;
        Ok(ClipboardContent::Text(
            String::from_utf8_lossy(&bytes).into_owned(),
        ))
    }

    fn write(&self, text: &str) -> Result<(), ClipboardError> {
        let text: Arc<str> = text.into();
        let source = self.manager.create_data_source(&self.qh, text.clone());
        for mime in TEXT_TYPES {
            source.offer((*mime).into());
        }
        self.device.set_selection(Some(&source));
        // The old source is cancelled by the compositor and destroyed then.
        self.inner.lock().unwrap().owned = Some((source, text));
        self.conn
            .flush()
            .map_err(|e| ClipboardError::Backend(e.to_string()))
    }
}

/// Reads to EOF, up to `MAX_BYTES`, giving up after `READ_TIMEOUT` in all.
fn read_pipe(fd: OwnedFd) -> Result<Vec<u8>, ClipboardError> {
    let deadline = Instant::now() + READ_TIMEOUT;
    let mut file = File::from(fd);
    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(ClipboardError::Timeout);
        }
        let timeout = Timespec {
            tv_sec: left.as_secs() as _,
            tv_nsec: left.subsec_nanos() as _,
        };
        let mut fds = [PollFd::new(&file, PollFlags::IN)];
        match poll(&mut fds, Some(&timeout)) {
            Ok(0) => return Err(ClipboardError::Timeout),
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(ClipboardError::Backend(e.to_string())),
        }
        let n = file
            .read(&mut buf)
            .map_err(|e| ClipboardError::Backend(e.to_string()))?;
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buf[..n]);
        if out.len() >= MAX_BYTES {
            out.truncate(MAX_BYTES);
            return Ok(out);
        }
    }
}

struct State {
    inner: Arc<Mutex<Inner>>,
}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrDataControlDeviceV1,
        event: device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            device::Event::Selection { id } => {
                let old = std::mem::replace(&mut state.inner.lock().unwrap().selection, id);
                if let Some(old) = old {
                    old.destroy();
                }
            }
            device::Event::Finished => {
                tracing::warn!("clipboard: compositor stopped the data device");
                let mut inner = state.inner.lock().unwrap();
                inner.finished = true;
                if let Some(old) = inner.selection.take() {
                    old.destroy();
                }
            }
            _ => {}
        }
    }

    event_created_child!(State, ZwlrDataControlDeviceV1, [
        device::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, Types::default()),
    ]);
}

impl Dispatch<ZwlrDataControlOfferV1, Types> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrDataControlOfferV1,
        event: offer::Event,
        types: &Types,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let offer::Event::Offer { mime_type } = event {
            types.lock().unwrap().push(mime_type);
        }
    }
}

impl Dispatch<ZwlrDataControlSourceV1, Arc<str>> for State {
    fn event(
        state: &mut Self,
        source: &ZwlrDataControlSourceV1,
        event: source::Event,
        text: &Arc<str>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            source::Event::Send { fd, .. } => {
                // A slow reader must not stall the Wayland thread.
                let text = text.clone();
                std::thread::spawn(move || {
                    let _ = File::from(fd).write_all(text.as_bytes());
                });
            }
            source::Event::Cancelled => {
                let mut inner = state.inner.lock().unwrap();
                if inner.owned.as_ref().is_some_and(|(s, _)| s == source) {
                    inner.owned = None;
                }
                source.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrDataControlManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrDataControlManagerV1,
        _: manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_pipe_stops_at_eof_and_times_out() {
        let (r, w) = pipe_with(PipeFlags::CLOEXEC).unwrap();
        File::from(w).write_all(b"hi").unwrap();
        assert_eq!(read_pipe(r).unwrap(), b"hi");

        // A source that never closes its end.
        let (r, _w) = pipe_with(PipeFlags::CLOEXEC).unwrap();
        assert!(matches!(read_pipe(r), Err(ClipboardError::Timeout)));
    }
}
