//! Open windows over a long-lived connection on the `vivus-wayland` thread.
//! wlr-foreign-toplevel-management gives the list, state and actions. Without
//! it, ext-foreign-toplevel-list gives a read-only list. Neither protocol links
//! its handles to the other's, so we never pair them.

use std::sync::{Arc, Mutex};

use anyhow::Context;
use vivus_core::{Window, WindowAction, WindowBackend, WindowError};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self as wlr_handle, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self as wlr_manager, ZwlrForeignToplevelManagerV1},
};

/// Called with a window each time it gains focus (from the Wayland thread).
pub type OnFocus = Box<dyn Fn(&Window) + Send>;

/// Tracks the compositor's windows; cheap to share behind an `Arc`.
pub struct Windows {
    conn: Connection,
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    windows: Vec<Tracked>,
    seat: Option<WlSeat>,
    next_id: u64,
    /// Bumped on each focus change, to order windows by recency.
    focus_clock: u64,
    /// The initial list has been read; windows from then on count as new.
    synced: bool,
}

struct Tracked {
    handle: Handle,
    /// What the compositor last committed with `done`.
    current: Option<Window>,
    pending: Window,
    focused_at: u64,
}

enum Handle {
    Wlr(ZwlrForeignToplevelHandleV1),
    Ext(ExtForeignToplevelHandleV1),
}

impl Handle {
    fn id(&self) -> wayland_client::backend::ObjectId {
        match self {
            Self::Wlr(h) => h.id(),
            Self::Ext(h) => h.id(),
        }
    }
}

impl Windows {
    /// Connects and reads the current windows before returning.
    pub fn connect(on_focus: OnFocus) -> anyhow::Result<Self> {
        let conn = Connection::connect_to_env().context("no Wayland display")?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let inner = Arc::new(Mutex::new(Inner::default()));
        let mut state = State {
            inner: inner.clone(),
            on_focus,
        };
        if let Ok(manager) = globals.bind::<ZwlrForeignToplevelManagerV1, _, _>(&qh, 1..=3, ()) {
            // Activation needs a seat; a compositor without one can't focus anything anyway.
            inner.lock().unwrap().seat = globals.bind::<WlSeat, _, _>(&qh, 1..=1, ()).ok();
            tracing::info!(
                "windows: wlr-foreign-toplevel-management v{}",
                manager.version()
            );
        } else {
            globals
                .bind::<ExtForeignToplevelListV1, _, _>(&qh, 1..=1, ())
                .context("compositor has no foreign-toplevel protocol")?;
            tracing::info!("windows: ext-foreign-toplevel-list (read-only, no active window)");
        }
        // Toplevel handles arrive in the first roundtrip, their state in the second.
        queue.roundtrip(&mut state)?;
        queue.roundtrip(&mut state)?;
        inner.lock().unwrap().synced = true;
        std::thread::Builder::new()
            .name("vivus-wayland".into())
            .spawn(move || {
                loop {
                    if let Err(e) = queue.blocking_dispatch(&mut state) {
                        tracing::error!("windows: Wayland connection lost: {e}");
                        state.inner.lock().unwrap().windows.clear();
                        return;
                    }
                }
            })
            .context("spawning wayland thread")?;
        Ok(Self { conn, inner })
    }
}

impl WindowBackend for Windows {
    fn windows(&self) -> Vec<Window> {
        let inner = self.inner.lock().unwrap();
        let mut v: Vec<(u64, Window)> = inner
            .windows
            .iter()
            .filter_map(|t| Some((t.focused_at, t.current.clone()?)))
            .collect();
        // Most compositors report one active window; it leads regardless of the clock.
        v.sort_by_key(|(at, w)| std::cmp::Reverse((w.active == Some(true), *at, w.id)));
        v.into_iter().map(|(_, w)| w).collect()
    }

    fn act(&self, id: u64, action: WindowAction) -> Result<(), WindowError> {
        {
            let inner = self.inner.lock().unwrap();
            let t = inner
                .windows
                .iter()
                .find(|t| t.current.as_ref().is_some_and(|w| w.id == id))
                .ok_or(WindowError::Gone)?;
            let Handle::Wlr(h) = &t.handle else {
                return Err(WindowError::Unsupported(action));
            };
            match action {
                WindowAction::Activate => {
                    let seat = inner
                        .seat
                        .as_ref()
                        .ok_or(WindowError::Unsupported(action))?;
                    if t.current.as_ref().is_some_and(|w| w.minimized) {
                        h.unset_minimized();
                    }
                    h.activate(seat);
                }
                WindowAction::Close => h.close(),
                WindowAction::Minimize => h.set_minimized(),
            }
        }
        self.conn
            .flush()
            .map_err(|e| WindowError::Backend(e.to_string()))
    }
}

struct State {
    inner: Arc<Mutex<Inner>>,
    on_focus: OnFocus,
}

impl State {
    fn add(&mut self, handle: Handle) {
        let mut inner = self.inner.lock().unwrap();
        inner.next_id += 1;
        let id = inner.next_id;
        inner.windows.push(Tracked {
            handle,
            current: None,
            pending: Window {
                id,
                ..Default::default()
            },
            focused_at: 0,
        });
    }

    fn with(&mut self, id: &wayland_client::backend::ObjectId, f: impl FnOnce(&mut Window)) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(t) = inner.windows.iter_mut().find(|t| t.handle.id() == *id) {
            f(&mut t.pending);
        }
    }

    fn done(&mut self, id: &wayland_client::backend::ObjectId) {
        let focused = {
            let mut inner = self.inner.lock().unwrap();
            let clock = inner.focus_clock + 1;
            let synced = inner.synced;
            let Some(t) = inner.windows.iter_mut().find(|t| t.handle.id() == *id) else {
                return;
            };
            let was_active = t.current.as_ref().and_then(|w| w.active) == Some(true);
            let is_active = t.pending.active == Some(true);
            let first = t.current.is_none() && synced;
            t.current = Some(t.pending.clone());
            let focused = (is_active && !was_active).then(|| t.pending.clone());
            // New windows count as recent even when they don't take focus.
            if focused.is_some() || first {
                t.focused_at = clock;
                inner.focus_clock = clock;
            }
            focused
        };
        if let Some(w) = focused {
            (self.on_focus)(&w);
        }
    }

    fn remove(&mut self, id: &wayland_client::backend::ObjectId) {
        self.inner
            .lock()
            .unwrap()
            .windows
            .retain(|t| t.handle.id() != *id);
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: wlr_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wlr_manager::Event::Toplevel { toplevel } => state.add(Handle::Wlr(toplevel)),
            wlr_manager::Event::Finished => tracing::warn!("windows: compositor stopped the list"),
            _ => {}
        }
    }

    event_created_child!(State, ZwlrForeignToplevelManagerV1, [
        wlr_manager::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: wlr_handle::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = handle.id();
        match event {
            wlr_handle::Event::Title { title } => state.with(&id, |w| w.title = title),
            wlr_handle::Event::AppId { app_id } => state.with(&id, |w| w.app_id = app_id),
            wlr_handle::Event::State { state: bytes } => {
                let flags: Vec<u32> = bytes
                    .chunks_exact(4)
                    .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                let has = |s: wlr_handle::State| flags.contains(&(s as u32));
                state.with(&id, |w| {
                    w.active = Some(has(wlr_handle::State::Activated));
                    w.minimized = has(wlr_handle::State::Minimized);
                    w.maximized = has(wlr_handle::State::Maximized);
                    w.fullscreen = has(wlr_handle::State::Fullscreen);
                });
            }
            wlr_handle::Event::Done => state.done(&id),
            wlr_handle::Event::Closed => {
                state.remove(&id);
                handle.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } => {
                state.add(Handle::Ext(toplevel))
            }
            ext_foreign_toplevel_list_v1::Event::Finished => {
                tracing::warn!("windows: compositor stopped the list")
            }
            _ => {}
        }
    }

    event_created_child!(State, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = handle.id();
        match event {
            ext_foreign_toplevel_handle_v1::Event::Title { title } => {
                state.with(&id, |w| w.title = title)
            }
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                state.with(&id, |w| w.app_id = app_id)
            }
            ext_foreign_toplevel_handle_v1::Event::Done => state.done(&id),
            ext_foreign_toplevel_handle_v1::Event::Closed => {
                state.remove(&id);
                handle.destroy();
            }
            _ => {}
        }
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
