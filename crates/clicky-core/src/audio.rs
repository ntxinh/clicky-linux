//! cpal output-stream management — owns the live `Stream` plus the shared
//! `Mixer`, and hands out the `TriggerProducer`/`Controls` handles the engine
//! and IPC threads use.
//!
//! Ownership: the mixer lives in `Arc<Mutex<Mixer>>` shared with the data
//! callback instead of moving the mixer into the closure. The brief's
//! alternative (registry in `Arc<RwLock>` outside, callback holding consumer +
//! voices) can't keep the rtrb ring alive across `set_device` — the consumer
//! dies with the closure and the producer returned by `start()` would push
//! into a dead ring. Sharing the whole mixer keeps producer, registry,
//! controls and stats persistent for free, and `register()`/`import_pack()`
//! keep their `&mut Mixer` API.
//!
//! RT path: the callback does `try_lock()` — no blocking, no allocation. A
//! contended callback (stream rebuild or a long `lock()` elsewhere) renders
//! one silent buffer. Device switch therefore keeps even in-flight voices
//! unless the negotiated sample rate changes, which resets voices via
//! `Mixer::set_sample_rate`.

use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, StreamConfig};
use parking_lot::{Mutex, MutexGuard};

use crate::mixer::{Controls, Mixer, Stats, TriggerProducer};

/// Alias for the mixer's remote control handle (name used by the plan/docs).
pub type MixerControls = Controls;

/// Output-device descriptor for the device picker.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeviceInfo {
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("no audio output device")]
    NoDevice,
    #[error("output device not found: {0}")]
    DeviceNotFound(String),
    #[error("cannot enumerate output devices: {0}")]
    Enumerate(#[source] cpal::DevicesError),
    #[error("no default output config: {0}")]
    Config(#[source] cpal::DefaultStreamConfigError),
    #[error("cannot list supported output configs: {0}")]
    Configs(#[source] cpal::SupportedStreamConfigsError),
    #[error("no f32 output config supported by device")]
    UnsupportedSampleFormat,
    #[error("output stream build failed: {0}")]
    BuildStream(#[source] cpal::BuildStreamError),
    #[error("output stream start failed: {0}")]
    PlayStream(#[source] cpal::PlayStreamError),
}

/// Live output stream + the shared mixer. `start()` returns this together
/// with the producer (engine) and controls (IPC) handles.
pub struct Audio {
    mixer: Arc<Mutex<Mixer>>,
    controls: Controls,
    stream: cpal::Stream,
    device_name: String,
    out_rate: u32,
    channels: usize,
}

/// Buffer-size ladder, cheapest-first (see docs/AUDIO_ENGINE.md: Fixed(128)
/// negotiates cleanly on this PipeWire box; the rest are portability fallbacks).
const BUFFER_ATTEMPTS: [BufferSize; 4] = [
    BufferSize::Fixed(128),
    BufferSize::Fixed(64),
    BufferSize::Fixed(256),
    BufferSize::Default,
];

fn pick_device(device_name: Option<&str>) -> Result<Device, AudioError> {
    let host = cpal::default_host();
    match device_name {
        None => host.default_output_device().ok_or(AudioError::NoDevice),
        Some(want) => host
            .output_devices()
            .map_err(AudioError::Enumerate)?
            .find(|d| d.name().ok().as_deref() == Some(want))
            .ok_or_else(|| AudioError::DeviceNotFound(want.to_string())),
    }
}

/// Base stream config: the device default when it's f32, else the highest
/// supported f32 rate. Buffer size is overlaid per attempt.
fn base_config(device: &Device) -> Result<StreamConfig, AudioError> {
    let default = device.default_output_config().map_err(AudioError::Config)?;
    if default.sample_format() == cpal::SampleFormat::F32 {
        return Ok(default.config());
    }
    device
        .supported_output_configs()
        .map_err(AudioError::Configs)?
        .filter(|r| r.sample_format() == cpal::SampleFormat::F32)
        .max_by_key(|r| r.max_sample_rate().0)
        .map(|r| r.with_max_sample_rate().config())
        .ok_or(AudioError::UnsupportedSampleFormat)
}

fn build_stream(
    device: &Device,
    base: &StreamConfig,
    mixer: &Arc<Mutex<Mixer>>,
    channels: usize,
) -> Result<cpal::Stream, AudioError> {
    let mut last_err = None;
    for buffer_size in BUFFER_ATTEMPTS {
        let config = StreamConfig { buffer_size, ..base.clone() };
        let mx = mixer.clone();
        let result = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| match mx.try_lock() {
                Some(mut m) => m.render(data, channels),
                // Held by set_device/registration: emit silence for this
                // buffer rather than block the RT thread.
                None => data.fill(0.0),
            },
            |e| eprintln!("clicky: output stream error: {e}"),
            None,
        );
        match result {
            Ok(stream) => return Ok(stream),
            Err(e) => last_err = Some(e),
        }
    }
    // BUFFER_ATTEMPTS is non-empty; last_err is always Some here.
    Err(AudioError::BuildStream(last_err.expect("attempts exhausted")))
}

impl Audio {
    /// Build and start a stream on `device_name` (or the default device when
    /// `None`). Returns the mixer producer for the engine and the shared
    /// controls for IPC.
    pub fn start(
        device_name: Option<&str>,
    ) -> Result<(Audio, TriggerProducer, MixerControls), AudioError> {
        let device = pick_device(device_name)?;
        let base = base_config(&device)?;
        let out_rate = base.sample_rate.0;
        let channels = base.channels as usize;
        let device_name = device.name().unwrap_or_else(|_| "<unnamed>".into());

        let mut mixer = Mixer::new(out_rate);
        let producer = mixer.producer().expect("fresh mixer has producer");
        let controls = mixer.controls();
        let mixer = Arc::new(Mutex::new(mixer));

        let stream = build_stream(&device, &base, &mixer, channels)?;
        stream.play().map_err(AudioError::PlayStream)?;

        let audio = Audio { mixer, controls: controls.clone(), stream, device_name, out_rate, channels };
        Ok((audio, producer, controls))
    }

    /// Output devices on the default host; empty on enumeration failure.
    pub fn devices() -> Vec<DeviceInfo> {
        let host = cpal::default_host();
        let default = host.default_output_device().and_then(|d| d.name().ok());
        let Ok(devices) = host.output_devices() else {
            return Vec::new();
        };
        // cpal exposes no stable device identity — flag the first device whose
        // name matches the default's so duplicates aren't all marked default.
        let mut default_flagged = false;
        devices
            .filter_map(|d| {
                let name = d.name().ok()?;
                let is_default = !default_flagged && Some(&name) == default.as_ref();
                default_flagged |= is_default;
                Some(DeviceInfo { name, is_default })
            })
            .collect()
    }

    /// Switch output to a named device: build the new stream first, then swap
    /// (the old stream keeps playing until the new one is live — failure
    /// leaves the current device untouched). Registered samples, the trigger
    /// producer and controls all persist across the switch. If the new device
    /// negotiates a different sample rate, voices and queued triggers are
    /// cleared (`Mixer::set_sample_rate`); otherwise in-flight voices keep
    /// playing.
    pub fn set_device(&mut self, name: &str) -> Result<(), AudioError> {
        let device = pick_device(Some(name))?;
        let base = base_config(&device)?;
        let stream = build_stream(&device, &base, &self.mixer, base.channels as usize)?;
        stream.play().map_err(AudioError::PlayStream)?;

        let out_rate = base.sample_rate.0;
        if out_rate != self.out_rate {
            self.lock_mixer().set_sample_rate(out_rate);
        }
        self.stream = stream;
        self.device_name = device.name().unwrap_or_else(|_| "<unnamed>".into());
        self.out_rate = out_rate;
        self.channels = base.channels as usize;
        Ok(())
    }

    /// Shared mixer handle — `lock()` off the RT path to register samples,
    /// read stats or clear voices. Keep the guard brief: the render callback
    /// emits silence while it's held.
    pub fn mixer(&self) -> &Arc<Mutex<Mixer>> {
        &self.mixer
    }

    fn lock_mixer(&self) -> MutexGuard<'_, Mixer> {
        self.mixer.lock()
    }

    /// Master gain 0..=1 (3 ms slew inside the mixer).
    pub fn set_master(&mut self, gain: f32) {
        self.controls.set_gain(gain);
    }

    /// Global enable, ramped.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.controls.set_enabled(enabled);
    }

    /// Cumulative mixer counters (brief lock — off-RT callers only).
    pub fn stats(&self) -> Stats {
        self.lock_mixer().stats()
    }

    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Ordered buffer-size ladder used for stream negotiation (diagnostics).
    pub fn buffer_attempts() -> &'static [BufferSize] {
        &BUFFER_ATTEMPTS
    }
}

// cpal stamps `platform::Stream` !Send/!Sync for WASAPI parity; on the ALSA
// host (all we run on) `StreamInner` is Sync and the handle — a channel plus
// the trigger fd — is movable, so sharing `Audio` behind a Mutex in Tauri
// managed state is sound. No `Audio` method runs on the RT thread.
unsafe impl Send for Audio {}
unsafe impl Sync for Audio {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mixer::Trigger;
    use std::time::Duration;

    #[test]
    fn devices_enumerates_without_panic() {
        let devices = Audio::devices();
        // Whatever the box exposes: at most one device flagged default.
        assert!(devices.iter().filter(|d| d.is_default).count() <= 1);
    }

    #[test]
    fn start_with_bogus_device_name_errors() {
        match Audio::start(Some("clicky-nonexistent-device-zzz")) {
            Err(AudioError::DeviceNotFound(name)) => {
                assert_eq!(name, "clicky-nonexistent-device-zzz")
            }
            // A deviceless/headless box may fail earlier with NoDevice —
            // still the correct "can't start" shape.
            Err(AudioError::NoDevice) | Err(AudioError::Enumerate(_)) => {}
            Ok(_) => panic!("bogus device name must not resolve"),
            Err(e) => panic!("unexpected error variant: {e}"),
        }
    }

    #[test]
    fn set_device_with_bogus_name_errors() {
        let Ok((mut audio, _p, _c)) = Audio::start(None) else {
            return; // headless: nothing to switch from
        };
        assert!(matches!(
            audio.set_device("clicky-nonexistent-device-zzz"),
            Err(AudioError::DeviceNotFound(_))
        ));
    }


    /// Device hot-swap, single-device variant: `set_device` to the *current*
    /// device name must succeed and leave a playing stream (stream swap, not
    /// a restart-from-scratch). Unplug-follow needs a second physical device
    /// — covered in docs/TESTING.md §8 where hardware allows.
    #[test]
    #[ignore = "needs a real output device"]
    fn set_device_to_current_succeeds() {
        let Ok((mut audio, _p, _c)) = Audio::start(None) else {
            return; // headless: skip rather than fail
        };
        let name = audio.device_name().to_string();
        audio.set_device(&name).expect("switch to same device");
        assert_eq!(audio.device_name(), name);
        assert!(audio.out_rate() > 0);
    }

    /// Real-hardware path: stream plays, trigger enqueued through the
    /// returned producer renders, and `stats().rendered` grows.
    #[test]
    #[ignore = "needs a real output device"]
    fn real_stream_renders_enqueued_trigger() {
        let Ok((audio, mut producer, controls)) = Audio::start(None) else {
            return; // headless: skip rather than fail
        };
        // Register after start: exercises the shared-mixer path exactly as
        // profiles::load does in the daemon.
        let pcm: Arc<[f32]> = vec![0.4f32; 4800].into();
        let sid = audio.mixer().lock().register(pcm, 48000);
        assert_ne!(sid, u16::MAX);

        let before = audio.stats().rendered;
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            producer.push(Trigger { sample: sid, gain: 1.0, pan: 0.0, pitch: 1.0, tone: 0.0 })
        );
        controls.set_gain(1.0);
        std::thread::sleep(Duration::from_millis(300));
        let after = audio.stats().rendered;
        assert!(after > before, "rendered {before} -> {after}");
        assert!(audio.out_rate() > 0 && audio.channels() > 0);
        assert!(Audio::devices().iter().any(|d| d.name == audio.device_name()));
    }
}
