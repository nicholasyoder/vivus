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
}

enum Control {
    SetActive(bool),
    Quit,
}

/// A microphone stream on its own PipeWire thread. Starts inactive so the mic
/// is only open while something is listening.
pub struct Capture {
    ctl: pw::channel::Sender<Control>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// `on_frames` gets each captured buffer as 16 kHz mono samples, on the
    /// capture thread; keep it cheap (e.g. send to a channel).
    pub fn spawn(
        opts: CaptureOptions,
        on_frames: impl Fn(Vec<f32>) + Send + 'static,
    ) -> Result<Self, Error> {
        let (ctl_tx, ctl_rx) = pw::channel::channel::<Control>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), Error>>();

        let thread = std::thread::Builder::new()
            .name("vivus-capture".into())
            .spawn(move || {
                if let Err(e) = run(opts, on_frames, ctl_rx, &ready_tx) {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .expect("spawn capture thread");

        ready_rx.recv().map_err(|_| Error::ThreadDied)??;
        Ok(Self {
            ctl: ctl_tx,
            thread: Some(thread),
        })
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
    ready: &mpsc::Sender<Result<(), Error>>,
) -> Result<(), Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Communication",
        *pw::keys::APP_NAME => "vivus",
        *pw::keys::NODE_NAME => "vivus-capture",
    };
    if let Some(target) = &opts.target {
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

    // Fixed format: PipeWire's adapter resamples and downmixes for us.
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(SAMPLE_RATE);
    info.set_channels(1);
    let format = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: info.into(),
        }),
    )
    .expect("serialize audio format")
    .0
    .into_inner();
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

    let _ = ready.send(Ok(()));
    mainloop.run();
    Ok(())
}
