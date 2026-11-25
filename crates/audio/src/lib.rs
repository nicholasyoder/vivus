//! PipeWire audio capture and playback.

mod capture;
mod devices;
mod duck;
mod playback;

pub use capture::{Capture, CaptureOptions, ECHO_CANCEL_SOURCE, EchoCancelOptions};
pub use devices::{Device, Devices, Direction};
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

/// EnumFormat pod for F32 mono at `rate`. The MONO position matters: without
/// it WirePlumber links the one port to the first channel only (left speaker,
/// left mic) instead of letting the adapter up- or downmix.
fn mono_f32_format(rate: u32) -> Vec<u8> {
    use pipewire::spa;
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(rate);
    info.set_channels(1);
    let mut position = [0; spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
    info.set_position(position);
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: info.into(),
        }),
    )
    .expect("serialize audio format")
    .0
    .into_inner()
}
