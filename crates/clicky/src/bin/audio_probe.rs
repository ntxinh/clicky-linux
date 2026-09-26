//! audio_probe — cpal playback-latency spike on this box (PipeWire/ALSA).
//!
//! Renders a real 48 kHz mono WAV through `clicky_core::mixer::Mixer` inside
//! the cpal data callback, pushes one `Trigger` from the main thread, and
//! measures trigger → first-nonzero-output latency. Throwaway diagnostic;
//! not the production audio path.

use std::f32;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clicky_core::mixer::{Mixer, Trigger, TriggerProducer};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, StreamConfig};
use parking_lot::Mutex;

const WAV: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../sounds/thocky/press-01.wav");
const LOG_N: usize = 24;

// Wall-clock base: every Instant crosses the callback boundary as nanos
// since `base` (Instant::now() epoch on the calling thread clock).
static T0_NANOS: AtomicI64 = AtomicI64::new(-1);
static CB_COUNT: AtomicUsize = AtomicUsize::new(0);
static FIRST_HIT: AtomicBool = AtomicBool::new(false);
static HIT_CB_IDX: AtomicUsize = AtomicUsize::new(0);
static HIT_FRAME: AtomicUsize = AtomicUsize::new(0);
static HIT_CB_WALL_NANOS: AtomicI64 = AtomicI64::new(0);
static HIT_PLAYBACK_DELAY_NANOS: AtomicI64 = AtomicI64::new(0);

struct CbRec {
    frames: usize,
    wall_ms: f64,
    playback_delay_ms: f64,
}

fn wall_nanos(base: Instant) -> i64 {
    Instant::now().duration_since(base).as_nanos() as i64
}

/// Minimal PCM WAV decoder — 16-bit or 32-bit-float, downmixes to mono.
/// Enough for the probe; not the real loader.
fn load_wav(path: &str) -> (Vec<f32>, u32) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    assert!(&bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE", "not a WAV");
    let mut pos = 12usize;
    let (mut fmt, mut rate, mut ch, mut bits) = (0u16, 0u32, 0u16, 0u16);
    loop {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if id == b"fmt " {
            fmt = u16::from_le_bytes(bytes[pos + 8..pos + 10].try_into().unwrap());
            ch = u16::from_le_bytes(bytes[pos + 10..pos + 12].try_into().unwrap());
            rate = u32::from_le_bytes(bytes[pos + 12..pos + 16].try_into().unwrap());
            bits = u16::from_le_bytes(bytes[pos + 22..pos + 24].try_into().unwrap());
        } else if id == b"data" {
            let data = &bytes[pos + 8..pos + 8 + size];
            let frames = data.len() / ((bits as usize / 8) * ch as usize);
            let mut out = Vec::with_capacity(frames);
            for f in 0..frames {
                let mut acc = 0.0f32;
                for c in 0..ch as usize {
                    let off = (f * ch as usize + c) * (bits as usize / 8);
                    acc += match (fmt, bits) {
                        (1, 16) => {
                            i16::from_le_bytes(data[off..off + 2].try_into().unwrap()) as f32
                                / 32768.0
                        }
                        (3, 32) => f32::from_le_bytes(data[off..off + 4].try_into().unwrap()),
                        _ => panic!("unsupported WAV fmt={fmt} bits={bits}"),
                    };
                }
                out.push(acc / ch as f32);
            }
            return (out, rate);
        }
        pos += 8 + size + (size & 1);
    }
}

thread_local! {
    static PRODUCER: std::cell::RefCell<Option<TriggerProducer>> =
        const { std::cell::RefCell::new(None) };
}

fn try_build(
    device: &cpal::Device,
    config: &StreamConfig,
    wav: &Arc<[f32]>,
    wav_rate: u32,
    base: Instant,
    log: &Arc<Mutex<Vec<CbRec>>>,
) -> Result<cpal::Stream, String> {
    let mut mixer = Mixer::new(config.sample_rate.0);
    let sid = mixer.register(wav.clone(), wav_rate);
    if sid == u16::MAX {
        return Err("mixer rejected sample".into());
    }
    let producer = mixer.producer().expect("fresh mixer has producer");
    let channels = config.channels as usize;
    let log2 = log.clone();
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                let idx = CB_COUNT.fetch_add(1, Ordering::Relaxed);
                let now = wall_nanos(base);
                let ts = info.timestamp();
                let pd = ts.playback.duration_since(&ts.callback).unwrap_or_default();
                if idx < LOG_N {
                    if let Some(mut l) = log2.try_lock() {
                        l.push(CbRec {
                            frames: data.len() / channels,
                            wall_ms: now as f64 / 1e6,
                            playback_delay_ms: pd.as_nanos() as f64 / 1e6,
                        });
                    }
                }
                mixer.render(data, channels);
                if !FIRST_HIT.load(Ordering::Relaxed) {
                    if let Some(f) = data.iter().position(|s| s.abs() > 1e-4) {
                        if !FIRST_HIT.swap(true, Ordering::AcqRel) {
                            HIT_CB_IDX.store(idx, Ordering::Relaxed);
                            HIT_FRAME.store(f / channels, Ordering::Relaxed);
                            HIT_CB_WALL_NANOS.store(now, Ordering::Relaxed);
                            HIT_PLAYBACK_DELAY_NANOS
                                .store(pd.as_nanos() as i64, Ordering::Relaxed);
                        }
                    }
                }
            },
            |e| eprintln!("stream error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())?;
    PRODUCER.with(|p| *p.borrow_mut() = Some(producer));
    Ok(stream)
}

fn main() {
    let base = Instant::now();

    // --- Hosts -------------------------------------------------------------
    let hosts = cpal::available_hosts();
    println!("available hosts: {hosts:?}");
    for id in &hosts {
        match cpal::host_from_id(*id) {
            Ok(h) => println!(
                "  {id:?}: default output = {:?}",
                h.default_output_device().and_then(|d| d.name().ok())
            ),
            Err(e) => println!("  {id:?}: host_from_id failed: {e}"),
        }
    }

    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        println!("NO OUTPUT DEVICE — no audio device in this environment; cannot measure.");
        return;
    };
    let dev_name = device.name().unwrap_or_else(|_| "<unnamed>".into());
    println!("default output device: {dev_name}");

    let supported = match device.default_output_config() {
        Ok(c) => c,
        Err(e) => {
            println!("default_output_config failed: {e}");
            return;
        }
    };
    println!(
        "default config: {} ch, {} Hz, fmt {:?}, buffer {:?}",
        supported.channels(),
        supported.sample_rate().0,
        supported.sample_format(),
        supported.buffer_size(),
    );
    if supported.sample_format() != cpal::SampleFormat::F32 {
        println!("default format is not f32 — mixer renders f32; aborting probe.");
        return;
    }

    // --- WAV ---------------------------------------------------------------
    let (pcm, wav_rate) = load_wav(WAV);
    let wav: Arc<[f32]> = pcm.into();
    println!("wav: {WAV} — {} frames @ {wav_rate} Hz", wav.len());

    // --- Stream build with buffer-size fallback -----------------------------
    let rate = supported.sample_rate();
    let channels = supported.channels();
    let attempts = [
        BufferSize::Fixed(128),
        BufferSize::Fixed(64),
        BufferSize::Fixed(256),
        BufferSize::Default,
    ];
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut stream = None;
    let mut chosen = BufferSize::Default;
    for bs in attempts {
        let config = StreamConfig { channels, sample_rate: rate, buffer_size: bs };
        match try_build(&device, &config, &wav, wav_rate, base, &log) {
            Ok(s) => {
                println!("buffer {bs:?}: OK");
                chosen = bs;
                stream = Some(s);
                break;
            }
            Err(e) => println!("buffer {bs:?}: FAILED — {e}"),
        }
    }
    let Some(stream) = stream else {
        println!("no stream config worked; giving up.");
        return;
    };
    stream.play().expect("play");

    // Warm up ~200 ms, then fire one trigger and time it.
    std::thread::sleep(Duration::from_millis(200));
    let mut producer = PRODUCER.with(|p| p.borrow_mut().take()).expect("producer");
    let t0 = wall_nanos(base);
    T0_NANOS.store(t0, Ordering::Release);
    assert!(
        producer.push(Trigger { sample: 0, gain: 1.0, pan: 0.0, pitch: 1.0, tone: 0.0 }),
        "enqueue trigger failed"
    );

    std::thread::sleep(Duration::from_secs(3));

    // --- Report -------------------------------------------------------------
    let total_cbs = CB_COUNT.load(Ordering::Relaxed);
    let log = log.lock();
    println!("\n=== first {} of {total_cbs} callbacks ===", log.len());
    println!("{:>4} {:>7} {:>10} {:>14}", "idx", "frames", "wall_ms", "playback_dly");
    for (i, r) in log.iter().enumerate() {
        println!(
            "{i:>4} {:>7} {:>10.2} {:>12.2}ms",
            r.frames, r.wall_ms, r.playback_delay_ms
        );
    }

    println!("\n=== summary ===");
    println!("device:            {dev_name}");
    println!("host:              {hosts:?}");
    println!("negotiated buffer: {chosen:?}");
    println!("callbacks total:   {total_cbs}");
    if FIRST_HIT.load(Ordering::Relaxed) {
        let hit_wall = HIT_CB_WALL_NANOS.load(Ordering::Relaxed) - t0;
        let pd = HIT_PLAYBACK_DELAY_NANOS.load(Ordering::Relaxed) as f64 / 1e6;
        let frame_off = HIT_FRAME.load(Ordering::Relaxed) as f64 / rate.0 as f64 * 1000.0;
        let wall_ms = hit_wall as f64 / 1e6;
        println!(
            "trigger→first-nonzero-callback: {wall_ms:.2} ms (cb #{})",
            HIT_CB_IDX.load(Ordering::Relaxed)
        );
        println!("predicted playback delay:       {pd:.2} ms");
        println!("first nonzero frame offset:     {frame_off:.2} ms into buffer");
        println!("EST. trigger→DAC latency:       {:.2} ms", wall_ms + pd + frame_off);
    } else {
        println!("NO NONZERO OUTPUT — trigger never rendered (check mixer/enqueue).");
    }
}
