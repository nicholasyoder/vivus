use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use spa::pod::Pod;

use crate::Error;

#[derive(Debug, Clone)]
pub struct PlaybackOptions {
    /// PipeWire node name or serial to play to. `None` follows the default sink.
    pub target: Option<String>,
    /// Rate of the samples passed to [`Playback::play`]; PipeWire resamples.
    pub sample_rate: u32,
    /// Volume of other apps' audio while we play, relative to theirs; 1
    /// leaves it alone.
    pub duck: f32,
    /// Our stream's volume as mixers show it (cubic), 0 to 1.
    pub volume: f32,
}

type OnFinished = Arc<dyn Fn() + Send + Sync>;

enum Control {
    Play(Vec<f32>),
    End,
    Stop,
    Pause(bool),
    UserTalking(bool),
    SetDuck(f32),
    SetVolume(f32),
    Quit,
}

/// Sends audio to a [`Playback`] from any thread. Shared, so clones follow
/// the stream when [`Playback::set_target`] replaces it.
#[derive(Clone)]
pub struct PlaybackSender(Arc<Mutex<pw::channel::Sender<Control>>>);

impl PlaybackSender {
    fn send(&self, c: Control) {
        let _ = self.0.lock().unwrap().send(c);
    }

    /// Queue mono samples after whatever is already queued.
    pub fn play(&self, samples: Vec<f32>) {
        self.send(Control::Play(samples));
    }

    /// Nothing more is coming: call `on_finished` once the queue has played out.
    pub fn end(&self) {
        self.send(Control::End);
    }

    /// Drop queued audio and go quiet now. Cancels a pending `end`.
    pub fn stop(&self) {
        self.send(Control::Stop);
    }

    /// Hold the queue and play silence until `resume` (or `stop`).
    pub fn pause(&self) {
        self.send(Control::Pause(true));
    }

    pub fn resume(&self) {
        self.send(Control::Pause(false));
    }

    /// Duck other audio while the user talks, whether or not we're playing:
    /// less music in the mic means less for echo cancellation to remove.
    pub fn user_talking(&self, talking: bool) {
        self.send(Control::UserTalking(talking));
    }
}

/// An output stream on its own PipeWire thread. Inactive (and so not holding
/// the sink awake) except while there is audio to play.
pub struct Playback {
    tx: PlaybackSender,
    thread: Option<JoinHandle<()>>,
    opts: PlaybackOptions,
    on_finished: OnFinished,
}

impl Playback {
    /// `on_finished` runs on the PipeWire thread after an `end()`ed queue drains.
    pub fn spawn(
        opts: PlaybackOptions,
        on_finished: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let on_finished: OnFinished = Arc::new(on_finished);
        let (ctl_tx, thread) = start(opts.clone(), on_finished.clone())?;
        Ok(Self {
            tx: PlaybackSender(Arc::new(Mutex::new(ctl_tx))),
            thread: Some(thread),
            opts,
            on_finished,
        })
    }

    pub fn sender(&self) -> PlaybackSender {
        self.tx.clone()
    }

    /// Plays to another sink from now on. The stream is recreated, so what's
    /// queued is dropped (and `on_finished` never comes for it).
    pub fn set_target(&mut self, target: Option<String>) -> Result<(), Error> {
        self.opts.target = target;
        self.quit();
        let (ctl_tx, thread) = start(self.opts.clone(), self.on_finished.clone())?;
        *self.tx.0.lock().unwrap() = ctl_tx;
        self.thread = Some(thread);
        Ok(())
    }

    pub fn set_duck(&mut self, level: f32) {
        self.opts.duck = level;
        self.tx.send(Control::SetDuck(level));
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.opts.volume = volume;
        self.tx.send(Control::SetVolume(volume));
    }

    fn quit(&mut self) {
        self.tx.send(Control::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn start(
    opts: PlaybackOptions,
    on_finished: OnFinished,
) -> Result<(pw::channel::Sender<Control>, JoinHandle<()>), Error> {
    let (ctl_tx, ctl_rx) = pw::channel::channel::<Control>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), Error>>();
    let thread = std::thread::Builder::new()
        .name("vivus-playback".into())
        .spawn(move || {
            if let Err(e) = run(opts, move || on_finished(), ctl_rx, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })
        .expect("spawn playback thread");
    ready_rx.recv().map_err(|_| Error::ThreadDied)??;
    Ok((ctl_tx, thread))
}

impl std::ops::Deref for Playback {
    type Target = PlaybackSender;
    fn deref(&self) -> &PlaybackSender {
        &self.tx
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.quit();
    }
}

#[derive(Default)]
struct State {
    queue: VecDeque<f32>,
    active: bool,
    paused: bool,
    /// `end()` received: finish once the queue is empty.
    ending: bool,
    /// Queue empty after `end()`; waiting for PipeWire's `drained`.
    draining: bool,
    /// Frames of underrun (silence) played in a row.
    silent: usize,
}

/// Silence after which other apps come back up mid-reply (e.g. while an
/// escalation runs after the filler). Shorter gaps between sentences keep
/// them ducked, so music doesn't bob up and down.
const UNDUCK_AFTER_SECS: usize = 1;

fn run(
    opts: PlaybackOptions,
    on_finished: impl Fn() + 'static,
    ctl_rx: pw::channel::Receiver<Control>,
    ready: &mpsc::Sender<Result<(), Error>>,
) -> Result<(), Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let ducker = match crate::duck::Ducker::new(&core, opts.duck) {
        Ok(d) => Some(Rc::new(d)),
        Err(e) => {
            tracing::warn!("ducking other audio unavailable: {e}");
            None
        }
    };
    // Ducked while we play or while the user talks.
    let (playing, talking) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
    let apply = {
        let (playing, talking, ducker) = (playing.clone(), talking.clone(), ducker.clone());
        move || {
            if let Some(d) = &ducker {
                d.set(playing.get() || talking.get());
            }
        }
    };
    let apply = Rc::new(apply);
    let duck = {
        let apply = apply.clone();
        move |on: bool| {
            playing.set(on);
            apply();
        }
    };
    let duck = Rc::new(duck);

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Playback",
        *pw::keys::MEDIA_ROLE => "Communication",
        *pw::keys::APP_NAME => "vivus",
        *pw::keys::NODE_NAME => "vivus-playback",
    };
    if let Some(target) = &opts.target {
        props.insert(*pw::keys::TARGET_OBJECT, target.as_str());
    }

    let stream = pw::stream::StreamRc::new(core.clone(), "vivus-playback", props)?;
    let state = Rc::new(RefCell::new(State::default()));
    let volume = Rc::new(Cell::new(opts.volume));
    let set_volume = |stream: &pw::stream::Stream, v: f32| {
        let linear = v.clamp(0.0, 1.0).powi(3);
        if let Err(e) = stream.set_control(spa::sys::SPA_PROP_channelVolumes, &[linear]) {
            tracing::warn!("playback volume: {e}");
        }
    };
    let on_finished = Rc::new(on_finished);

    let process_state = state.clone();
    let drained_state = state.clone();
    let drained_duck = duck.clone();
    let process_duck = duck.clone();
    let unduck_frames = opts.sample_rate as usize * UNDUCK_AFTER_SECS;
    let _listener = stream
        .add_local_listener::<()>()
        .state_changed({
            let volume = volume.clone();
            // Each time it starts, so ours wins over a volume restored for the app.
            move |stream, _, old, new| {
                tracing::debug!("playback stream: {old:?} -> {new:?}");
                if matches!(
                    new,
                    pw::stream::StreamState::Paused | pw::stream::StreamState::Streaming
                ) {
                    set_volume(stream, volume.get());
                }
            }
        })
        .process(move |stream, _| {
            let mut st = process_state.borrow_mut();
            if st.draining {
                return;
            }
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let requested = buffer.requested() as usize;
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let frames = match data.data() {
                Some(bytes) => {
                    let max = bytes.len() / 4;
                    let n = if requested > 0 {
                        requested.min(max)
                    } else {
                        max
                    };
                    // Underruns between sentences play as silence.
                    for out in bytes[..n * 4].chunks_exact_mut(4) {
                        let s = match st.paused {
                            true => 0.0,
                            false => st.queue.pop_front().unwrap_or(0.0),
                        };
                        out.copy_from_slice(&s.to_le_bytes());
                    }
                    if st.queue.is_empty() && !st.paused {
                        st.silent += n;
                        if st.silent >= unduck_frames {
                            process_duck(false);
                        }
                    } else {
                        st.silent = 0;
                    }
                    n
                }
                None => 0,
            };
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = 4;
            *chunk.size_mut() = (frames * 4) as u32;
            drop(buffer);

            if st.ending && st.queue.is_empty() && !st.paused {
                st.draining = true;
                if let Err(e) = stream.flush(true) {
                    tracing::warn!("playback drain: {e}");
                }
            }
        })
        .drained({
            let on_finished = on_finished.clone();
            move |stream, _| {
                let mut st = drained_state.borrow_mut();
                if !st.draining {
                    return;
                }
                st.draining = false;
                st.ending = false;
                st.active = false;
                if let Err(e) = stream.set_active(false) {
                    tracing::warn!("playback set_active(false): {e}");
                }
                drained_duck(false);
                drop(st);
                on_finished();
            }
        })
        .register()?;

    let format = crate::mono_f32_format(opts.sample_rate);
    let mut params = [Pod::from_bytes(&format).expect("audio format pod")];

    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::INACTIVE,
        &mut params,
    )?;

    // On quit, the restored volumes of ducked streams must reach the server.
    let quit_seq = Rc::new(std::cell::Cell::new(None));
    let synced = Rc::new(std::cell::Cell::new(false));
    let _core_listener = core
        .add_listener_local()
        .error(|id, _, res, msg| tracing::debug!("playback: pipewire error on {id} ({res}): {msg}"))
        .done({
            let (quit_seq, synced) = (quit_seq.clone(), synced.clone());
            move |id, seq| {
                if id == pw::core::PW_ID_CORE && quit_seq.get() == Some(seq) {
                    synced.set(true);
                }
            }
        })
        .register();

    let weak_loop = mainloop.downgrade();
    let ctl_stream = stream.clone();
    let _ctl = ctl_rx.attach(mainloop.loop_(), move |msg| {
        let mut st = state.borrow_mut();
        let set_active = |st: &mut State, active: bool| {
            if st.active != active {
                st.active = active;
                if let Err(e) = ctl_stream.set_active(active) {
                    tracing::warn!("playback set_active({active}): {e}");
                }
                duck(active);
            }
        };
        match msg {
            Control::Play(samples) => {
                st.queue.extend(samples);
                st.silent = 0;
                set_active(&mut st, true);
                duck(true); // again, if a long gap brought the others back up
            }
            Control::End if !st.active && st.queue.is_empty() => {
                drop(st);
                on_finished();
            }
            Control::End => st.ending = true,
            Control::Pause(paused) => st.paused = paused,
            Control::UserTalking(on) => {
                talking.set(on);
                apply();
            }
            Control::SetDuck(level) => {
                if let Some(d) = &ducker {
                    d.set_level(level);
                }
            }
            Control::SetVolume(v) => {
                volume.set(v);
                set_volume(&ctl_stream, v);
            }
            Control::Stop => {
                st.queue.clear();
                st.ending = false;
                st.paused = false;
                if st.draining {
                    st.draining = false;
                    let _ = ctl_stream.flush(false);
                }
                set_active(&mut st, false);
            }
            Control::Quit => {
                talking.set(false);
                duck(false);
                quit_seq.set(core.sync(0).ok());
                if let Some(l) = weak_loop.upgrade() {
                    l.quit();
                }
            }
        }
    });

    let _ = ready.send(Ok(()));
    mainloop.run();
    // Bounded, so a stuck server can't keep the daemon from exiting.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while !synced.get() && std::time::Instant::now() < deadline {
        let step = std::time::Duration::from_millis(20);
        mainloop.loop_().iterate(pw::loop_::Timeout::Finite(step));
    }
    tracing::debug!("playback: quit (volumes synced: {})", synced.get());
    Ok(())
}
