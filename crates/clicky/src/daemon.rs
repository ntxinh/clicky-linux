//! Headless daemon: engine loop + Unix control socket.
//!
//! Boot order: config → staged manifest → audio stream → register samples →
//! input thread → control socket. Missing evdev access is degraded, never
//! fatal: the daemon serves enable/disable/profile/status/quit while
//! `input::run`'s sustained retry poll picks the nodes up once permissions
//! land (the udev rule emits no hotplug event for already-enumerated nodes).
//!
//! Socket: `$XDG_RUNTIME_DIR/clicky/control.sock` — one line per connection,
//! one reply line back. `status` replies with a JSON object; everything else
//! replies `ok`/`err <msg>`. This is the future DMS-keybind/tray path (T14).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clicky_core::audio::{Audio, MixerControls};
use clicky_core::config::Store;
use clicky_core::engine::Engine;
use clicky_core::input::{self, InputEvent};
use clicky_core::normalizer::Normalizer;
use clicky_core::profiles;
use parking_lot::Mutex;

/// Signal-safe shutdown flag; the accept loop polls it (nonblocking accept).
static SIG_QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: i32) {
    SIG_QUIT.store(true, Ordering::Relaxed);
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
    }
}

fn socket_dir() -> PathBuf {
    directories::BaseDirs::new()
        .and_then(|b| b.runtime_dir().map(|d| d.join("clicky")))
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/clicky-{}", unsafe { libc::getuid() })))
}

/// `$XDG_RUNTIME_DIR/clicky/control.sock`.
pub fn socket_path() -> PathBuf {
    socket_dir().join("control.sock")
}

/// `~/.local/share/clicky/sounds`, falling back to `./sounds` for dev runs.
fn sounds_dir() -> PathBuf {
    if let Some(p) = directories::ProjectDirs::from("io", "clicky", "clicky") {
        let d = p.data_dir().join("sounds");
        if d.join("profiles.json").exists() {
            return d;
        }
    }
    PathBuf::from("sounds")
}

/// State mutated by the socket handler (main thread) and read by the input
/// thread: engine for `on_key`, store for `config.enabled`/saves.
struct Shared {
    engine: Engine,
    store: Store,
}

/// Run the daemon in the foreground until `quit`, SIGTERM or SIGINT.
/// Returns the process exit code.
pub fn run() -> i32 {
    install_signal_handlers();

    let store = match Store::load(Store::default_path()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("clicky: config load failed: {e}");
            return 1;
        }
    };
    let sounds = sounds_dir();
    // Stage outside the mixer lock; the stream is up by register time but
    // startup contention is nil.
    let staged = match profiles::stage_manifest(&sounds) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("clicky: manifest {}: {e}", sounds.display());
            return 1;
        }
    };
    let (audio, producer, controls) =
        match Audio::start(store.cfg.sound.output_device_uid.as_deref()) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("clicky: audio start failed: {e}");
                return 1;
            }
        };
    let report = {
        let mut mixer = audio.mixer().lock();
        profiles::register_manifest(staged, &mut mixer)
    };
    for (id, e) in &report.errors {
        eprintln!("clicky: profile {id}: {e}");
    }
    eprintln!(
        "clicky: {} profile(s) loaded from {}; audio on {} ({} Hz, {} ch)",
        report.profiles.len(),
        sounds.display(),
        audio.device_name(),
        audio.out_rate(),
        audio.channels(),
    );
    let profile_ids: Vec<String> = report.profiles.iter().map(|p| p.id.clone()).collect();

    controls.set_enabled(store.cfg.enabled);
    let engine = Engine::new(store.cfg.clone(), report.profiles, producer);
    let shared = Arc::new(Mutex::new(Shared { engine, store }));

    // Input thread: enumerate → run loop. EACCES-only enumeration leaves the
    // daemon headless-but-alive; run()'s retry poll picks nodes up when perms
    // land.
    let enumerated = input::keyboard_devices();
    eprintln!(
        "clicky: capture: {} keyboard(s), {} issue(s)",
        enumerated.keyboards.len(),
        enumerated.issues.len(),
    );
    for i in &enumerated.issues {
        eprintln!("clicky: {}: {}", i.path.display(), i.error);
    }
    let quit = Arc::new(AtomicBool::new(false));
    let input_handle = spawn_input(enumerated.keyboards, shared.clone(), quit.clone());

    // Control socket: stale socket file gets reclaimed; a live one means a
    // second daemon — exit rather than double-consume evdev.
    let sock_dir = socket_dir();
    if let Err(e) = std::fs::create_dir_all(&sock_dir) {
        eprintln!("clicky: {}: {e}", sock_dir.display());
        return 1;
    }
    let sock = socket_path();
    if UnixStream::connect(&sock).is_ok() {
        eprintln!("clicky: another daemon owns {}", sock.display());
        return 1;
    }
    let _ = std::fs::remove_file(&sock);
    let listener = match UnixListener::bind(&sock) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("clicky: bind {}: {e}", sock.display());
            return 1;
        }
    };
    if let Err(e) = listener.set_nonblocking(true) {
        eprintln!("clicky: socket nonblocking: {e}");
        return 1;
    }
    eprintln!("clicky: control socket on {}", sock.display());

    while !quit.load(Ordering::Relaxed) && !SIG_QUIT.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => handle(stream, &shared, &controls, &audio, &profile_ids, &quit),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                eprintln!("clicky: accept: {e}");
                break;
            }
        }
    }

    quit.store(true, Ordering::Relaxed);
    let _ = input_handle.join();
    let mut sh = shared.lock();
    if let Err(e) = sh.store.flush() {
        eprintln!("clicky: config save: {e}");
    }
    let _ = std::fs::remove_file(&sock);
    eprintln!("clicky: quit");
    0
}

/// Input thread body: evdev → Normalizer → Engine (held in `shared`).
fn spawn_input(
    devices: Vec<input::HotDevice>,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut normalizer = Normalizer::new();
        let result = input::run(
            devices,
            |ev| match ev {
                InputEvent::Key {
                    device,
                    code,
                    value,
                } => {
                    if let Some(kev) = normalizer.feed(device, code, value, false) {
                        shared.lock().engine.on_key(&kev);
                    }
                }
                InputEvent::Dropped { device } => normalizer.clear_held(device),
            },
            stop,
        );
        if let Err(e) = result {
            eprintln!("clicky: input loop failed: {e}");
        }
    })
}

/// One connection = one command line = one reply line.
fn handle(
    mut stream: UnixStream,
    shared: &Arc<Mutex<Shared>>,
    controls: &MixerControls,
    audio: &Audio,
    profile_ids: &[String],
    quit: &Arc<AtomicBool>,
) {
    // ponytail: sequential accept loop — a silent client would stall control;
    // 5 s is generous for a fire-and-forget CLI.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    let read = stream
        .try_clone()
        .and_then(|s| BufReader::new(s).read_line(&mut line));
    let reply = match read {
        Err(_) => "err read".to_string(),
        Ok(0) => "err empty".to_string(),
        Ok(_) => command(line.trim(), shared, controls, audio, profile_ids, quit),
    };
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.write_all(b"\n");
}


/// Dispatch one command; returns the reply line (no newline).
fn command(
    line: &str,
    shared: &Arc<Mutex<Shared>>,
    controls: &MixerControls,
    audio: &Audio,
    profile_ids: &[String],
    quit: &Arc<AtomicBool>,
) -> String {
    let mut words = line.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some("status"), None, None) => status_line(shared, audio),
        (Some("quit"), None, None) => {
            quit.store(true, Ordering::Relaxed);
            "ok".into()
        }
        (Some(cmd @ ("enable" | "disable")), None, None) => {
            let mut sh = shared.lock();
            let mut cfg = sh.store.cfg.clone();
            cfg.enabled = cmd == "enable";
            if let Err(e) = sh.store.save_debounced(&cfg) {
                return format!("err {e}");
            }
            sh.engine.update_config(cfg);
            controls.set_enabled(cmd == "enable");
            "ok".into()
        }
        (Some("profile"), Some(id), None) => {
            if !profile_ids.iter().any(|p| p == id) {
                return format!("err unknown profile {id}");
            }
            let mut sh = shared.lock();
            let mut cfg = sh.store.cfg.clone();
            cfg.sound.profile_id = id.to_string();
            if let Err(e) = sh.store.save_debounced(&cfg) {
                return format!("err {e}");
            }
            sh.engine.update_config(cfg);
            "ok".into()
        }
        _ => "err unknown command".to_string(),
    }
}

/// `status` reply: one JSON line.
fn status_line(shared: &Arc<Mutex<Shared>>, audio: &Audio) -> String {
    // Live re-enumerate — the input thread keeps no readable status surface.
    let scan = input::keyboard_devices();
    let stats = audio.stats();
    let sh = shared.lock();
    serde_json::json!({
        "enabled": sh.store.cfg.enabled,
        "profile": sh.engine.profile_id(),
        "audio_device": audio.device_name(),
        "capture": if !scan.keyboards.is_empty() {
            "live"
        } else if scan.issues.is_empty() {
            "no-keyboards"
        } else {
            "denied"
        },
        "keyboards": scan.keyboards.len(),
        "issues": scan.issues.len(),
        "accepted": stats.accepted,
        "dropped": stats.dropped,
        "stolen": stats.stolen,
    })
    .to_string()
}
