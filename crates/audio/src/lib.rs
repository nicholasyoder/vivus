//! PipeWire audio capture and playback.

mod capture;
mod playback;

pub use capture::{Capture, CaptureOptions};
pub use playback::{Playback, PlaybackOptions, PlaybackSender};

/// Capture always delivers 16 kHz mono f32; PipeWire converts from the device format.
pub const SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("pipewire: {0}")]
    PipeWire(#[from] pipewire::Error),
    #[error("audio thread exited during setup")]
    ThreadDied,
}
