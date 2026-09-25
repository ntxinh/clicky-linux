//! Engine boot shared by the headless daemon (`run()`) and the Tauri app
//! (`ipc.rs`), plus the daemon's Unix control socket.
//!
//! Boot order: config → staged manifest → audio stream → register samples →
//! input thread. Missing evdev access is degraded, never fatal: the daemon
//! serves enable/disable/profile/status/quit while `input::run`'s sustained
//! retry poll picks the nodes up once permissions land (the udev rule emits
//! no hotplug event for already-enumerated nodes).
//!
//! Socket: `$XDG_RUNTIME_DIR/clicky/control.sock` — one line per connection,
//! one reply line back. `status` replies with a JSON object; everything else
//! replies `ok`/`err <msg>`.

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

/// `~/.local/share/clicky/sounds`, falling back to `./sounds` for dev runs
/// (and `../../sounds` under `tauri dev`, which starts in crates/clicky).
pub fn sounds_dir() -> PathBuf {
    if let Some(p) = directories::ProjectDirs::from("io", "clicky", "clicky") {
        let d = p.data_dir().join("sounds");
        if d.join("profiles.json").exists() {
            return d;
        }
    }
    for cand in [PathBuf::from("sounds"), PathBuf::from("../../sounds")] {
        if cand.join("profiles.json").exists() {
            return cand;
        }
    }
    PathBuf::from("sounds")
}

/// State mutated by the socket handler (main thread), IPC commands and the
/// input thread: engine for `on_key`/`update_config`, store for saves.
pub struct Shared {
    pub engine: Engine,
    pub store: Store,
}

/// Everything [`boot`] produced that a caller needs past startup.
pub struct Boot {
    /// Engine + store behind one lock (IPC thread ≠ audio thread).
    pub shared: Arc<Mutex<Shared>>,
    /// Live output stream + shared mixer.
    pub audio: Audio,
    /// Remote gain/enable handle into the running mixer.
    pub controls: MixerControls,
    /// Where `profiles.json` was found; packs import into `<dir>/user/`.
    pub sounds_dir: PathBuf,
    /// Set to stop the input loop; join `input` before exit.
    pub quit: Arc<AtomicBool>,
    /// Input-thread liveness (`capture` field of status/diagnostics).
    pub capture_alive: Arc<AtomicBool>,
    /// evdev → Normalizer → Engine reader thread.
    pub input: Option<std::thread::JoinHandle<()>>,
}

/// Shared startup: load config, stage + register the manifest, start audio,
/// spawn the input thread. Used by both `run()` (daemon) and `ipc::run_app()`
/// (Tauri). Returns a description of the failure — callers print and exit.
pub fn boot() -> Result<Boot, String> {
    let store = Store::load(Store::default_path()).map_err(|e| format!("config load failed: {e}"))?;
    let sounds = sounds_dir();
    // Stage outside the mixer lock; the stream is up by register time but
    // startup contention is nil.
    let staged = profiles::stage_manifest(&sounds)
        .map_err(|e| format!("manifest {}: {e}", sounds.display()))?;
    let (audio, producer, controls) = Audio::start(store.cfg.sound.output_device_uid.as_deref())
        .map_err(|e| format!("audio start failed: {e}"))?;
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

    controls.set_enabled(store.cfg.enabled);
    let engine = Engine::new(store.cfg.clone(), report.profiles, producer);
    let shared = Arc::new(Mutex::new(Shared { engine, store }));

    // Input thread: enumerate → run loop. EACCES-only enumeration leaves the
    // app headless-but-alive; run()'s retry poll picks nodes up when perms
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
    // Input-thread liveness for status: set inside the reader loop, cleared
    // when it exits — a dead loop must never report `capture: "live"`.
    let capture_alive = Arc::new(AtomicBool::new(false));
    let input = spawn_input(
        enumerated.keyboards,
        shared.clone(),
        quit.clone(),
        capture_alive.clone(),
    );

    Ok(Boot {
        shared,
        audio,
        controls,
        sounds_dir: sounds,
        quit,
        capture_alive,
        input: Some(input),
    })
}

/// Stop the input thread and flush the store. Safe to call once at shutdown.
pub fn shutdown(boot: &mut Boot) {
    boot.quit.store(true, Ordering::Relaxed);
    if let Some(h) = boot.input.take() {
        let _ = h.join();
    }
    if let Err(e) = boot.shared.lock().store.flush() {
        eprintln!("clicky: config save: {e}");
    }
}

/// Run the daemon in the foreground until `quit`, SIGTERM or SIGINT.
/// Returns the process exit code.
pub fn run() -> i32 {
    install_signal_handlers();

    let mut b = match boot() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("clicky: {e}");
            return 1;
        }
    };

    // Control socket: stale socket file gets reclaimed; a live one means a
    // second daemon — exit rather than double-consume evdev.
    let sock_dir = socket_dir();
    if let Err(e) = std::fs::create_dir_all(&sock_dir) {
        eprintln!("clicky: {}: {e}", sock_dir.display());
        return 1;
    }
    // The /tmp fallback dir would land at umask-0755 — lock it down so no
    // other user can pre-bind the socket, and refuse a foreign-owned dir.
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = match std::fs::metadata(&sock_dir) {
        Ok(m) if m.uid() == unsafe { libc::getuid() } => m,
        Ok(_) => {
            eprintln!("clicky: {} not owned by us", sock_dir.display());
            return 1;
        }
        Err(e) => {
            eprintln!("clicky: {}: {e}", sock_dir.display());
            return 1;
        }
    };
    let mut perms = meta.permissions();
    perms.set_mode(0o700);
    if let Err(e) = std::fs::set_permissions(&sock_dir, perms) {
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

    while !b.quit.load(Ordering::Relaxed) && !SIG_QUIT.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => handle(
                stream,
                &b.shared,
                &b.controls,
                &b.audio,
                &b.quit,
                &b.capture_alive,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                eprintln!("clicky: accept: {e}");
                break;
            }
        }
    }

    shutdown(&mut b);
    let _ = std::fs::remove_file(&sock);
    eprintln!("clicky: quit");
    0
}

/// Input thread body: evdev → Normalizer → Engine (held in `shared`).
fn spawn_input(
    devices: Vec<input::HotDevice>,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut normalizer = Normalizer::new();
        alive.store(true, Ordering::Relaxed);
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
        alive.store(false, Ordering::Relaxed);
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
    quit: &Arc<AtomicBool>,
    capture_alive: &Arc<AtomicBool>,
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
        Ok(_) => command(line.trim(), shared, controls, audio, quit, capture_alive),
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
    quit: &Arc<AtomicBool>,
    capture_alive: &Arc<AtomicBool>,
) -> String {
    let mut words = line.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some("status"), None, None) => status_line(shared, audio, capture_alive),
        (Some("quit"), None, None) => {
            quit.store(true, Ordering::Relaxed);
            "ok".into()
        }
        (Some(cmd @ ("enable" | "disable")), None, None) => {
            let enable = cmd == "enable";
            let mut sh = shared.lock();
            let mut cfg = sh.store.cfg.clone();
            let prev_enabled = cfg.enabled;
            cfg.enabled = enable;
            // Apply to the live path first; a failed persist reverts engine,
            // mixer and store so `err` genuinely means nothing changed.
            sh.engine.update_config(cfg.clone());
            controls.set_enabled(enable);
            if let Err(e) = sh.store.save_debounced(&cfg) {
                sh.store.cfg.enabled = prev_enabled;
                let prev_cfg = sh.store.cfg.clone();
                sh.engine.update_config(prev_cfg);
                controls.set_enabled(prev_enabled);
                return format!("err {e}");
            }
            "ok".into()
        }
        (Some("profile"), Some(id), None) => {
            let mut sh = shared.lock();
            if !sh
                .engine
                .profiles()
                .iter()
                .any(|p| p.id == *id)
            {
                return format!("err unknown profile {id}");
            }
            let mut cfg = sh.store.cfg.clone();
            let prev = cfg.sound.profile_id.clone();
            cfg.sound.profile_id = id.to_string();
            sh.engine.update_config(cfg.clone());
            if let Err(e) = sh.store.save_debounced(&cfg) {
                sh.store.cfg.sound.profile_id = prev;
                let prev_cfg = sh.store.cfg.clone();
                sh.engine.update_config(prev_cfg);
                return format!("err {e}");
            }
            "ok".into()
        }
        _ => "err unknown command".to_string(),
    }
}

/// `status` reply: one JSON line.
fn status_line(shared: &Arc<Mutex<Shared>>, audio: &Audio, alive: &Arc<AtomicBool>) -> String {
    // Enumerate readable nodes for counts; the reader's own liveness flag
    // decides "live" vs "dead" so a crashed loop never lies.
    let scan = input::keyboard_devices();
    let alive = alive.load(Ordering::Relaxed);
    let stats = audio.stats();
    let sh = shared.lock();
    serde_json::json!({
        "enabled": sh.store.cfg.enabled,
        "profile": sh.engine.profile_id(),
        "audio_device": audio.device_name(),
        "capture": if !alive {
            "dead"
        } else if !scan.keyboards.is_empty() {
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
