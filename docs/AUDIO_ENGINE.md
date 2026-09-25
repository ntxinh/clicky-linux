# Clicky Linux — Audio Engine

Condensed from design spec §3. Rust port of Clicky's `CClickyAudio.c` mixer;
cpal output on PipeWire. The render callback is hard-RT: no allocation,
no locks — triggers arrive fully resolved.

## Mixer spec

| Parameter | Value |
|---|---|
| Voices | 96 (oldest-voice steal when full) |
| Trigger queue | SPSC (rtrb), capacity 1024, drop-oldest + overflow counter |
| Registered samples | max 2048, `Arc<[f32]>` mono + source rate |
| Resample | linear interpolation, `step = srcRate/outRate × pitch` |
| Tone | one-pole: `tone<0` → LP cutoff `18000×0.06^{-tone}`; `tone>0` → brightness `+80%×(dry−filtered)`; 0 = bypass |
| Pan | constant-power, `θ = (pan+1)×π/4` |
| Ramps | per-voice 0.5 ms attack / 2 ms end-release; master gain/enable 3 ms slew (no pops) |
| Limiter | transparent below −3.1 dBFS, knee to 0.9999 FS |
| Stats | rendered / accepted / dropped / stolen / active |

`Trigger{sample_idx, gain, pan, pitch, tone}`.

## Gain stack (from Clicky DSP.swift)

```
gain = sliderVolume × manifestGain × normalization × variation × calibration(×2)
```

Typing path adds per-key override volume, modifier gain, home-row softness.
Recorded releases ×0.7 (≈−3.1 dB). Variation per press: ±2.5% pitch +
0.94–1.0 gain jitter. Pitch slider: `speed = 2^{pitch×0.5}`,
pitch ∈ [−1,1] (±half octave), clamp 0.25–4.

## Normalization

Per-sample correction
`clamp(pow(targetRMS/(rms×profileGain), 0.75), 0.5, min(1.8, 0.65/(peak×profileGain)))`;
target = median press RMS across non-silent profiles clamped 0.04–0.12
(fallback 0.06); press/release pair shares one correction bounded by both
peaks; `normalizationReference=false` excludes a pack from the median.

## Loading & switching

All pack samples preloaded + decoded to mono f32 at load/profile switch.
Profile switch = atomic pointer swap → instant. No disk I/O or decode per
press. Release variant chosen at press time, stored per-key.

## Devices

cpal default output → PipeWire; `StreamConfig` from device default; request
~128-frame buffer where honored. Device picker over IPC; stream rebuild on
device loss with debounce (in-flight sounds cut — accepted).

## Measured latency

Measured 2026-09-26 by `cargo run -p clicky --bin audio_probe` (spike:
`sounds/thocky/press-01.wav` via `Mixer::producer()` → `Trigger` → `render()`
in the cpal callback). Fedora 44, PipeWire via pipewire-alsa.

| Field | Value |
|---|---|
| cpal hosts offered | `[Alsa]` only (no pulse/jack in this build) |
| Default device | `default` (pipewire-alsa → PipeWire) |
| Default config | 2 ch, 44100 Hz, f32, buffer range 1..=4194304 |
| Negotiated request | `BufferSize::Fixed(128)` — accepted on first try |
| Frames/callback | ~59 (mostly 59, some 58; PipeWire resample 44.1k→48k quantum) |
| Playback delay reported | 2.7–2.9 ms/callback (`playback − callback`) |
| Trigger→first-nonzero cb | 0.41 ms |
| **Est. trigger→DAC latency** | **≈3.3 ms** (queueing 0.4 + playback delay 2.9) |

`Fixed(128)` was honored — no fallback needed; `Fixed(64)`/`Fixed(256)`/
`Default` untested-as-needed. Even the spec target buffer easily beats the
<10 ms budget; 3.3 ms estimate excludes the evdev→enqueue hop (~µs–ms).

Note: at 44.1 kHz device rate a 128-frame *request* yields ~59-frame
callbacks — PipeWire keeps its own ~64-frame @48 kHz quantum and slices the
stream buffer. If a device negotiates at 48 kHz directly, expect ~128.
