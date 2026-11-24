use std::sync::mpsc;
use std::thread::JoinHandle;

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use spa::pod::Pod;

use crate::{Error, SAMPLE_RATE};

#[derive(Debug, Default, Clone)]
pub struct CaptureOptions {
    /// PipeWire node name or serial to capture from (e.g. the echo-cancel
    /// source). `None` follows the default source.
    pub target: Option<String>,
    /// Run the mic through PipeWire's echo canceller, loaded into this process.
    pub echo_cancel: Option<EchoCancelOptions>,
}

#[derive(Debug, Default, Clone)]
pub struct EchoCancelOptions {
    /// Sink whose output is cancelled; `None` follows the default sink, so
    /// everything the speakers play (music too) is removed, not just our speech.
    pub reference: Option<String>,
}

/// Name of the echo-cancelled source node while it exists.
pub const ECHO_CANCEL_SOURCE: &str = "vivus-echo-cancel-source";

enum Control {
    SetActive(bool),
    Quit,
}

/// A microphone stream on its own PipeWire thread. Starts inactive so the mic
/// is only open while something is listening.
pub struct Capture {
    ctl: pw::channel::Sender<Control>,
    thread: Option<JoinHandle<()>>,
    echo_cancelled: bool,
}

impl Capture {
    /// `on_frames` gets each captured buffer as 16 kHz mono samples, on the
    /// capture thread; keep it cheap (e.g. send to a channel).
    pub fn spawn(
        opts: CaptureOptions,
        on_frames: impl Fn(Vec<f32>) + Send + 'static,
    ) -> Result<Self, Error> {
        let (ctl_tx, ctl_rx) = pw::channel::channel::<Control>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<bool, Error>>();

        let thread = std::thread::Builder::new()
            .name("vivus-capture".into())
            .spawn(move || {
                if let Err(e) = run(opts, on_frames, ctl_rx, &ready_tx) {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .expect("spawn capture thread");

        let echo_cancelled = ready_rx.recv().map_err(|_| Error::ThreadDied)??;
        Ok(Self {
            ctl: ctl_tx,
            thread: Some(thread),
            echo_cancelled,
        })
    }

    /// Whether the echo canceller loaded (it was requested and PipeWire has it).
    pub fn echo_cancelled(&self) -> bool {
        self.echo_cancelled
    }

    pub fn set_active(&self, active: bool) {
        let _ = self.ctl.send(Control::SetActive(active));
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.ctl.send(Control::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(
    opts: CaptureOptions,
    on_frames: impl Fn(Vec<f32>) + 'static,
    ctl_rx: pw::channel::Receiver<Control>,
    ready: &mpsc::Sender<Result<bool, Error>>,
) -> Result<(), Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    // The module lives in our context, so it goes away with the daemon.
    let echo_cancelled = opts
        .echo_cancel
        .as_ref()
        .is_some_and(|ec| load_echo_cancel(&context, opts.target.as_deref(), ec));

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Communication",
        *pw::keys::APP_NAME => "vivus",
        *pw::keys::NODE_NAME => "vivus-capture",
    };
    if echo_cancelled {
        props.insert(*pw::keys::TARGET_OBJECT, ECHO_CANCEL_SOURCE);
    } else if let Some(target) = &opts.target {
        props.insert(*pw::keys::TARGET_OBJECT, target.as_str());
    }

    let stream = pw::stream::StreamRc::new(core, "vivus-capture", props)?;

    let _listener = stream
        .add_local_listener::<()>()
        .state_changed(|_, _, old, new| tracing::debug!("capture stream: {old:?} -> {new:?}"))
        .process(move |stream, _| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let offset = data.chunk().offset() as usize;
            let size = data.chunk().size() as usize;
            let Some(bytes) = data.data() else {
                return;
            };
            let Some(bytes) = bytes.get(offset..offset + size) else {
                return;
            };
            let samples = bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            on_frames(samples);
        })
        .register()?;

    let format = crate::mono_f32_format(SAMPLE_RATE);
    let mut params = [Pod::from_bytes(&format).expect("audio format pod")];

    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::INACTIVE,
        &mut params,
    )?;

    let weak_loop = mainloop.downgrade();
    let ctl_stream = stream.clone();
    let _ctl = ctl_rx.attach(mainloop.loop_(), move |msg| match msg {
        Control::SetActive(active) => {
            if let Err(e) = ctl_stream.set_active(active) {
                tracing::warn!("capture set_active({active}): {e}");
            }
        }
        Control::Quit => {
            if let Some(l) = weak_loop.upgrade() {
                l.quit();
            }
        }
    });

    let _ = ready.send(Ok(echo_cancelled));
    mainloop.run();
    Ok(())
}

/// Loads `libpipewire-module-echo-cancel` in monitor mode: the reference is
/// the sink's monitor, so nothing has to be routed through a special sink.
/// Its nodes stay suspended, and the mic closed, while our stream is inactive.
fn load_echo_cancel(
    context: &pw::context::ContextRc,
    mic: Option<&str>,
    opts: &EchoCancelOptions,
) -> bool {
    let target = |t: Option<&str>| {
        t.map(|t| format!("target.object = {}", quote(t)))
            .unwrap_or_default()
    };
    let args = format!(
        "{{ monitor.mode = true audio.channels = 1 audio.position = [ MONO ] \
         node.description = \"Vivus echo cancellation\" \
         capture.props = {{ node.name = \"vivus-echo-cancel-capture\" {} }} \
         source.props = {{ node.name = \"{ECHO_CANCEL_SOURCE}\" priority.session = 0 }} \
         sink.props = {{ node.name = \"vivus-echo-cancel-reference\" {} }} }}",
        target(mic),
        target(opts.reference.as_deref()),
    );
    let name = c"libpipewire-module-echo-cancel";
    let args = std::ffi::CString::new(args).expect("no NUL in module args");
    // SAFETY: valid context on its own loop thread; the module is owned by the
    // context and destroyed with it.
    let module = unsafe {
        pw::sys::pw_context_load_module(
            context.as_raw_ptr(),
            name.as_ptr(),
            args.as_ptr(),
            std::ptr::null_mut(),
        )
    };
    if module.is_null() {
        let err = std::io::Error::last_os_error();
        tracing::warn!("echo cancellation unavailable, loading {name:?}: {err}");
        return false;
    }
    tracing::info!("echo cancellation loaded (source {ECHO_CANCEL_SOURCE})");
    true
}

/// A SPA-JSON string literal.
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}
