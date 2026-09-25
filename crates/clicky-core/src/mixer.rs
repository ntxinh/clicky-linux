//! Lock-free RT mixer — direct port of Clicky's CClickyAudio.c.
//!
//! Ownership story: `Mixer` owns the consumer half of the SPSC trigger ring
//! (rtrb, cap [`QUEUE_CAP`]), the voice array and the sample registry. The
//! producer half starts embedded so `enqueue()` works for tests and offline
//! use; `producer()` hands a [`TriggerProducer`] to the engine thread before
//! the mixer itself is moved into the audio callback (`&mut self` methods
//! are then unreachable there — by design).
//!
//! `render()` touches only consumer + voices: no allocation, no locks, no
//! float panics.

use std::f32::consts::FRAC_PI_4;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

pub const MAX_VOICES: usize = 96;
pub const QUEUE_CAP: usize = 1024;
pub const MAX_SAMPLES: usize = 2048;

/// What the engine queues per key event. Fields are clamped on enqueue.
#[derive(Clone, Copy, Debug)]
pub struct Trigger {
    /// Index from [`Mixer::register`].
    pub sample: u16,
    /// 0..=4
    pub gain: f32,
    /// -1 (left) ..= 1 (right)
    pub pan: f32,
    /// Playback speed multiplier, 0.25..=4
    pub pitch: f32,
    /// -1 (warm low-pass) ..= 1 (bright high shelf)
    pub tone: f32,
}

impl Trigger {
    /// `None` when any field is non-finite or the sample index is unregistered.
    fn sanitized(mut self, sample_count: usize) -> Option<Trigger> {
        if self.sample as usize >= sample_count
            || !self.gain.is_finite()
            || !self.pan.is_finite()
            || !self.pitch.is_finite()
            || !self.tone.is_finite()
        {
            return None;
        }
        self.gain = self.gain.clamp(0.0, 4.0);
        self.pan = self.pan.clamp(-1.0, 1.0);
        self.pitch = self.pitch.clamp(0.25, 4.0);
        self.tone = self.tone.clamp(-1.0, 1.0);
        Some(self)
    }
}

/// Cumulative mixer counters (mirror of the C `CAStats` subset we expose).
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    /// Frames rendered.
    pub rendered: u64,
    /// Triggers accepted into the queue.
    pub accepted: u64,
    /// Triggers rejected because the queue was full.
    pub dropped: u64,
    /// Voices stolen because all [`MAX_VOICES`] were busy.
    pub stolen: u64,
}

/// Producer half for the engine thread. Take it via [`Mixer::producer()`]
/// before the mixer moves into the audio callback. Lock-free, allocation-free.
pub struct TriggerProducer {
    inner: rtrb::Producer<Trigger>,
    sample_count: Arc<AtomicUsize>,
    accepted: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
}

impl TriggerProducer {
    /// Sanitize and queue a trigger. `false` when the trigger is invalid or
    /// the queue is full (full queue also bumps [`Stats::dropped`]).
    pub fn push(&mut self, t: Trigger) -> bool {
        let Some(t) = t.sanitized(self.sample_count.load(Ordering::Acquire)) else {
            return false;
        };
        match self.inner.push(t) {
            Ok(()) => {
                self.accepted.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(rtrb::PushError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }
}

/// Remote-safe gain/enable switches, cloneable into the control thread while
/// the mixer itself lives inside the audio callback. Read with a 3 ms slew in
/// `render`.
#[derive(Clone)]
pub struct Controls {
    gain_bits: Arc<AtomicU32>,
    enabled: Arc<AtomicBool>,
}

impl Controls {
    /// Master gain 0..=1 (non-finite becomes 0).
    pub fn set_gain(&self, gain: f32) {
        let g = if gain.is_finite() { gain.clamp(0.0, 1.0) } else { 0.0 };
        self.gain_bits.store(g.to_bits(), Ordering::Relaxed);
    }
    /// Global enable, ramped.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }
}

struct Sample {
    pcm: Arc<[f32]>,
    rate: u32,
}

#[derive(Clone, Copy, Default)]
struct Voice {
    active: bool,
    sample: u16,
    pos: f64,
    step: f64,
    left: f32,
    right: f32,
    filter: f32,
    coeff: f32,
    brightness: f32,
    age: u64,
    elapsed: u32,
}

pub struct Mixer {
    producer: Option<TriggerProducer>,
    consumer: rtrb::Consumer<Trigger>,
    samples: Vec<Sample>,
    voices: [Voice; MAX_VOICES],
    /// Output sample rate — baked into voice steps and ramp lengths.
    rate: u32,
    gain_bits: Arc<AtomicU32>,
    enabled: Arc<AtomicBool>,
    current_gain: f32,
    current_enabled: f32,
    voice_age: u64,
    sample_count: Arc<AtomicUsize>,
    accepted: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    rendered: AtomicU64,
    stolen: AtomicU64,
}

impl Mixer {
    /// `out_rate` is the output device's sample rate (0 → 48000).
    pub fn new(out_rate: u32) -> Mixer {
        let (inner, consumer) = rtrb::RingBuffer::new(QUEUE_CAP);
        let sample_count = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        Mixer {
            producer: Some(TriggerProducer {
                inner,
                sample_count: sample_count.clone(),
                accepted: accepted.clone(),
                dropped: dropped.clone(),
            }),
            consumer,
            samples: Vec::new(),
            voices: [Voice::default(); MAX_VOICES],
            rate: if out_rate > 0 { out_rate } else { 48000 },
            gain_bits: Arc::new(AtomicU32::new(0.4f32.to_bits())),
            enabled: Arc::new(AtomicBool::new(true)),
            current_gain: 0.4,
            current_enabled: 1.0,
            voice_age: 0,
            sample_count,
            accepted,
            dropped,
            rendered: AtomicU64::new(0),
            stolen: AtomicU64::new(0),
        }
    }

    /// Take the producer half for the engine thread. Afterwards `enqueue()`
    /// returns `false` — use `TriggerProducer::push` instead.
    pub fn producer(&mut self) -> Option<TriggerProducer> {
        self.producer.take()
    }

    /// Cloneable control handle — keeps working after the mixer is moved
    /// into the audio callback.
    pub fn controls(&self) -> Controls {
        Controls { gain_bits: self.gain_bits.clone(), enabled: self.enabled.clone() }
    }

    pub fn set_gain(&self, gain: f32) {
        self.controls().set_gain(gain);
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.controls().set_enabled(enabled);
    }

    /// Register mono PCM (`Arc`, so the loader keeps its copy) at `src_rate`.
    /// Returns the sample id for [`Trigger::sample`], or `u16::MAX` when the
    /// registry is full or the input is unusable. PCM is assumed sanitized
    /// (finite, roughly [-1,1]) by the loader — the render path does not clamp.
    pub fn register(&mut self, pcm: Arc<[f32]>, src_rate: u32) -> u16 {
        if pcm.is_empty() || src_rate == 0 || self.samples.len() >= MAX_SAMPLES {
            return u16::MAX;
        }
        let id = self.samples.len() as u16;
        self.samples.push(Sample { pcm, rate: src_rate });
        self.sample_count.store(self.samples.len(), Ordering::Release);
        id
    }

    /// Samples registered so far (registry watermark).
    pub fn registered_count(&self) -> usize {
        self.samples.len()
    }

    /// Enqueue a trigger through the embedded producer (tests / offline use).
    /// `false` when invalid, the queue is full, or the producer was taken.
    pub fn enqueue(&mut self, t: Trigger) -> bool {
        match self.producer.as_mut() {
            Some(p) => p.push(t),
            None => false,
        }
    }

    /// Silence all voices and drop pending triggers. Call while render is stopped.
    pub fn clear(&mut self) {
        self.voices = [Voice::default(); MAX_VOICES];
        while self.consumer.pop().is_ok() {}
    }

    /// Change output rate; clears voices and pending triggers.
    pub fn set_sample_rate(&mut self, rate: u32) {
        if rate > 0 {
            self.rate = rate;
            self.clear();
        }
    }

    pub fn stats(&self) -> Stats {
        Stats {
            rendered: self.rendered.load(Ordering::Relaxed),
            accepted: self.accepted.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            stolen: self.stolen.load(Ordering::Relaxed),
        }
    }

    /// Interleaved render into `out` (`frames * channels` samples).
    /// channels 1 = mono downmix, 2 = stereo, >2 = stereo + zeros.
    /// No allocation, no locks — safe inside the RT callback.
    pub fn render(&mut self, out: &mut [f32], channels: usize) {
        if channels == 0 {
            return;
        }
        let frames = out.len() / channels;
        while let Ok(t) = self.consumer.pop() {
            self.start_voice(t);
        }
        let target_gain = f32::from_bits(self.gain_bits.load(Ordering::Relaxed));
        let target_enabled = if self.enabled.load(Ordering::Relaxed) { 1.0 } else { 0.0 };
        let rate = self.rate as f32;
        let slew = 1.0 - (-1.0 / (0.003 * rate)).exp();
        let attack_frames = (rate * 0.0005).max(1.0);
        let release_frames = (rate * 0.002).max(1.0);
        for frame in 0..frames {
            self.current_enabled += slew * (target_enabled - self.current_enabled);
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for v in &mut self.voices {
                if !v.active {
                    continue;
                }
                let s = &self.samples[v.sample as usize];
                let index = v.pos as usize;
                if index >= s.pcm.len() {
                    v.active = false;
                    continue;
                }
                let frac = (v.pos - index as f64) as f32;
                let a = s.pcm[index];
                let b = if index + 1 < s.pcm.len() { s.pcm[index + 1] } else { 0.0 };
                let mut value = a + (b - a) * frac;
                // One-pole tone filter.
                v.filter += v.coeff * (value - v.filter);
                if v.brightness < 0.0 {
                    value = v.filter;
                } else if v.brightness > 0.0 {
                    value += v.brightness * (value - v.filter) * 0.8;
                }
                v.elapsed += 1;
                let attack = (v.elapsed as f32 / attack_frames).min(1.0);
                let remaining = ((s.pcm.len() as f64 - v.pos) / v.step) as f32;
                let release = (remaining / release_frames).min(1.0);
                value *= attack * release * self.current_enabled;
                l += value * v.left;
                r += value * v.right;
                v.pos += v.step;
            }
            self.current_gain += slew * (target_gain - self.current_gain);
            let base = frame * channels;
            if channels == 1 {
                out[base] = limit((l + r) * 0.5 * self.current_gain);
            } else {
                out[base] = limit(l * self.current_gain);
                out[base + 1] = limit(r * self.current_gain);
                for c in &mut out[base + 2..base + channels] {
                    *c = 0.0;
                }
            }
        }
        self.rendered.fetch_add(frames as u64, Ordering::Relaxed);
    }

    /// Allocate a free voice, stealing the oldest when all are busy.
    fn start_voice(&mut self, t: Trigger) {
        let mut selected = 0;
        let mut oldest = u64::MAX;
        for (i, v) in self.voices.iter().enumerate() {
            if !v.active {
                selected = i;
                oldest = u64::MAX;
                break;
            }
            if v.age < oldest {
                oldest = v.age;
                selected = i;
            }
        }
        if oldest != u64::MAX {
            self.stolen.fetch_add(1, Ordering::Relaxed);
        }
        let s = &self.samples[t.sample as usize];
        let angle = (t.pan + 1.0) * FRAC_PI_4;
        // Neutral/bright voices share one fixed LP used as the shelf reference;
        // dark voices get cutoff 18000 * 0.06^(-tone).
        let cutoff = if t.tone < 0.0 { 18000.0 * 0.06f32.powf(-t.tone) } else { 1800.0 };
        let coeff = 1.0 - (-std::f32::consts::TAU * cutoff.min(self.rate as f32 * 0.45) / self.rate as f32).exp();
        self.voice_age += 1;
        self.voices[selected] = Voice {
            active: true,
            sample: t.sample,
            pos: 0.0,
            step: s.rate as f64 / self.rate as f64 * t.pitch as f64,
            left: angle.cos() * t.gain,
            right: angle.sin() * t.gain,
            filter: 0.0,
            coeff,
            brightness: t.tone,
            age: self.voice_age,
            elapsed: 0,
        };
    }
}

/// Soft limiter — transparent below −3.1 dBFS (~0.7); continuous soft knee to 0.9999.
fn limit(sample: f32) -> f32 {
    let magnitude = sample.abs();
    if magnitude <= 0.7 {
        return sample;
    }
    let limited = 0.7 + 0.3 * ((magnitude - 0.7) / (magnitude - 0.4));
    limited.min(0.9999).copysign(sample)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const RATE: u32 = 48_000;

    fn sine(freq: f32, secs: f32) -> Arc<[f32]> {
        (0..(RATE as f32 * secs) as usize)
            .map(|i| 0.5 * (TAU * freq * i as f32 / RATE as f32).sin())
            .collect()
    }

    fn noise(secs: f32) -> Arc<[f32]> {
        let mut rng = fastrand::Rng::with_seed(7);
        (0..(RATE as f32 * secs) as usize).map(|_| rng.f32() - 0.5).collect()
    }

    fn trig(sample: u16) -> Trigger {
        Trigger { sample, gain: 1.0, pan: 0.0, pitch: 1.0, tone: 0.0 }
    }

    fn rms(buf: &[f32]) -> f32 {
        (buf.iter().map(|x| x * x).sum::<f32>() / buf.len() as f32).sqrt()
    }

    /// Naive DFT magnitude centroid (bin index) — test-only, correctness over speed.
    fn centroid(buf: &[f32]) -> f64 {
        let n = buf.len();
        let (mut num, mut den) = (0.0f64, 0.0f64);
        for k in 1..n / 2 {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (i, &x) in buf.iter().enumerate() {
                let w = -2.0 * std::f64::consts::PI * k as f64 * i as f64 / n as f64;
                re += x as f64 * w.cos();
                im += x as f64 * w.sin();
            }
            let m = (re * re + im * im).sqrt();
            num += k as f64 * m;
            den += m;
        }
        num / den
    }

    #[test]
    fn silence_in_silence_out() {
        let mut m = Mixer::new(RATE);
        let mut out = vec![7.0f32; 512 * 2]; // must be overwritten
        m.render(&mut out, 2);
        assert!(out.iter().all(|&x| x == 0.0));
        assert_eq!(m.stats().rendered, 512);
    }

    #[test]
    fn sine_trigger_produces_nonzero_output() {
        let mut m = Mixer::new(RATE);
        let s = m.register(sine(440.0, 1.0), RATE);
        assert!(m.enqueue(trig(s)));
        let mut out = vec![0.0f32; 1024 * 2];
        m.render(&mut out, 2);
        assert!(rms(&out) > 0.01, "rms {}", rms(&out));
    }

    #[test]
    fn ninety_seventh_trigger_steals_oldest() {
        let mut m = Mixer::new(RATE);
        let s = m.register(sine(440.0, 2.0), RATE);
        for _ in 0..MAX_VOICES + 1 {
            assert!(m.enqueue(trig(s)));
        }
        let mut out = vec![0.0f32; 256 * 2];
        m.render(&mut out, 2);
        assert_eq!(m.stats().stolen, 1);
        assert!(rms(&out) > 0.0);
    }

    #[test]
    fn hard_pan_puts_energy_on_one_channel() {
        for (pan, live, dead) in [(-1.0f32, 0usize, 1usize), (1.0, 1, 0)] {
            let mut m = Mixer::new(RATE);
            let s = m.register(sine(440.0, 0.5), RATE);
            assert!(m.enqueue(Trigger { pan, ..trig(s) }));
            let mut out = vec![0.0f32; 1024 * 2];
            m.render(&mut out, 2);
            let live_e: f32 = out.iter().skip(live).step_by(2).map(|x| x * x).sum();
            let dead_e: f32 = out.iter().skip(dead).step_by(2).map(|x| x * x).sum();
            assert!(live_e > 0.1, "pan {pan}: live energy {live_e}");
            // f32 cos(pi) leaves ~1e-7 residue — assert relative silence, not exact 0.
            assert!(dead_e < live_e * 1e-8, "pan {pan}: dead channel {dead_e} vs live {live_e}");
        }
    }

    #[test]
    fn limiter_caps_at_ceiling_with_all_voices() {
        let mut m = Mixer::new(RATE);
        let s = m.register(sine(440.0, 1.0), RATE);
        for _ in 0..MAX_VOICES {
            assert!(m.enqueue(Trigger { gain: 4.0, ..trig(s) }));
        }
        let mut out = vec![0.0f32; 2048 * 2];
        m.render(&mut out, 2);
        let peak = out.iter().fold(0.0f32, |a, &x| a.max(x.abs()));
        assert!(peak <= 0.9999, "peak {peak} exceeded ceiling");
        assert!(peak > 0.9, "peak {peak}: limiter knee never engaged");
    }

    #[test]
    fn full_queue_rejects_and_counts_dropped() {
        let mut m = Mixer::new(RATE);
        let s = m.register(sine(440.0, 1.0), RATE);
        for _ in 0..QUEUE_CAP {
            assert!(m.enqueue(trig(s)));
        }
        assert!(!m.enqueue(trig(s)));
        assert!(!m.enqueue(trig(s)));
        assert_eq!(m.stats().dropped, 2);
        assert_eq!(m.stats().accepted, QUEUE_CAP as u64);
    }

    #[test]
    fn invalid_trigger_rejected_without_drop_count() {
        let mut m = Mixer::new(RATE);
        assert!(!m.enqueue(trig(0))); // unregistered sample
        let s = m.register(sine(440.0, 0.1), RATE);
        assert!(!m.enqueue(Trigger { gain: f32::NAN, ..trig(s) }));
        assert_eq!(m.stats().dropped, 0);
        assert_eq!(m.stats().accepted, 0);
    }

    #[test]
    fn tone_shifts_spectral_centroid() {
        let render_tone = |tone: f32| -> Vec<f32> {
            let mut m = Mixer::new(RATE);
            let s = m.register(noise(0.2), RATE);
            assert!(m.enqueue(Trigger { tone, ..trig(s) }));
            let mut out = vec![0.0f32; 2048 * 2];
            m.render(&mut out, 2);
            // Left channel, steady-state window away from attack/release.
            out.chunks_exact(2).map(|c| c[0]).collect::<Vec<_>>()[512..1536].to_vec()
        };
        let dark = centroid(&render_tone(-1.0));
        let neutral = centroid(&render_tone(0.0));
        let bright = centroid(&render_tone(1.0));
        assert!(dark < neutral, "dark {dark} !< neutral {neutral}");
        assert!(bright > neutral, "bright {bright} !> neutral {neutral}");
    }

    #[test]
    fn taken_producer_still_queues() {
        let mut m = Mixer::new(RATE);
        let s = m.register(sine(440.0, 0.5), RATE);
        let mut p = m.producer().unwrap();
        assert!(p.push(trig(s)));
        assert!(!m.enqueue(trig(s))); // producer is gone
        let mut out = vec![0.0f32; 512 * 2];
        m.render(&mut out, 2);
        assert!(rms(&out) > 0.01);
        assert_eq!(m.stats().accepted, 1);
    }
}
