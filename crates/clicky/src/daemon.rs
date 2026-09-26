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
use clicky_core::config::{Store, VisualizerSettings};
use clicky_core::engine::Engine;
use clicky_core::input::{self, InputEvent};
use clicky_core::normalizer::Normalizer;
use clicky_core::profiles;
use clicky_core::visual::VisualBus;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::process::{Child, Command};

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
/// (and `../../sounds` under `tauri dev`, which starts in crates/clicky),
/// then `/usr/share/clicky/sounds` for the RPM install.
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
    let system = PathBuf::from("/usr/share/clicky/sounds");
    if system.join("profiles.json").exists() {
        return system;
    }
    PathBuf::from("sounds")
}

/// One supervised `clicky-overlay <kind>` child. `restarts` counts the one
/// allowed respawn; `failed` freezes the entry so `status` can report it.
struct OverlayChild {
    kind: String,
    child: Option<Child>,
    restarts: u32,
    failed: bool,
}

/// Overlay supervision: spawn `clicky-overlay <kind>` for every enabled
/// visualizer kind, reap exits, allow ONE restart, then mark `failed` (the
/// `overlays` field of `status`). Kill on config-off and on shutdown.
#[derive(Default)]
pub struct Overlays {
    children: HashMap<String, OverlayChild>,
    status: HashMap<String, &'static str>,
}

impl Overlays {
    /// Reconcile running children with `vis` — call from any context that
    /// holds `shared` (accept loop, IPC mutate, diagnostics).
    pub fn sync(&mut self, vis: &VisualizerSettings) {
        for ch in self.children.values_mut() {
            let dead = ch
                .child
                .as_mut()
                .and_then(|c| c.try_wait().ok().flatten())
                .is_some();
            if !dead {
                continue;
            }
            ch.child = None;
            if ch.failed {
                continue;
            }
            let wanted = vis.enabled && vis.kinds.get(&ch.kind).copied().unwrap_or(false);
            if wanted && ch.restarts == 0 {
                ch.restarts = 1;
                eprintln!("clicky: overlay {} died; restarting once", ch.kind);
                match spawn_overlay(&ch.kind, vis) {
                    Ok(child) => ch.child = Some(child),
                    Err(e) => {
                        eprintln!("clicky: overlay {} respawn: {e}", ch.kind);
                        ch.failed = true;
                    }
                }
            } else if wanted {
                eprintln!("clicky: overlay {} failed after restart", ch.kind);
                ch.failed = true;
            }
        }
        // Drop entries for kinds no longer enabled (after reap, so a crash
        // on an enabled kind is reported before the entry goes away).
        let status = &mut self.status;
        self.children.retain(|k, ch| {
            if vis.enabled && vis.kinds.get(k).copied().unwrap_or(false) {
                status.insert(k.clone(), if ch.failed { "failed" } else { "running" });
                true
            } else {
                if let Some(mut c) = ch.child.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                status.remove(k);
                false
            }
        });
        // Kinds enabled but not yet running.
        let wanted: Vec<String> = vis
            .kinds
            .keys()
            .filter(|k| vis.enabled && vis.kinds[*k] && !self.children.contains_key(*k))
            .cloned()
            .collect();
        for kind in wanted {
            match spawn_overlay(&kind, vis) {
                Ok(child) => {
                    eprintln!("clicky: overlay {kind} spawned");
                    self.children.insert(
                        kind.clone(),
                        OverlayChild {
                            kind: kind.clone(),
                            child: Some(child),
                            restarts: 0,
                            failed: false,
                        },
                    );
                    self.status.insert(kind.clone(), "running");
                }
                Err(e) => {
                    // Record a failed child so the next sync doesn't retry —
                    // a spawn-erroring kind would otherwise fork+exec every
                    // 20 ms tick. Re-enable clears the mark via retain above.
                    eprintln!("clicky: overlay {kind}: {e}");
                    self.children.insert(
                        kind.clone(),
                        OverlayChild {
                            kind: kind.clone(),
                            child: None,
                            restarts: 0,
                            failed: true,
                        },
                    );
                    self.status.insert(kind.clone(), "failed");
                }
            }
        }
    }

    /// kind → "running" | "failed" for the status reply.
    pub fn status_map(&self) -> HashMap<String, &'static str> {
        self.status.clone()
    }

    /// Kill all children (shutdown path; the FIFO goes away with us anyway).
    pub fn kill_all(&mut self) {
        for ch in self.children.values_mut() {
            if let Some(mut c) = ch.child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
}

/// Prefer a `clicky-overlay` binary next to our own exe (cargo target dir);
/// fall back to PATH. `combo` gets the configured timeout via env.
fn spawn_overlay(kind: &str, vis: &VisualizerSettings) -> std::io::Result<Child> {
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("clicky-overlay")));
    let mut cmd = match sibling.filter(|p| p.is_file()) {
        Some(p) => Command::new(p),
        None => Command::new("clicky-overlay"),
    };
    cmd.arg(kind)
        .env("CLICKY_COMBO_TIMEOUT", format!("{}", vis.combo_timeout));
    cmd.spawn()
}

/// Names of currently-enabled overlay kinds — the per-kind FIFO set.
fn enabled_kinds(vis: &VisualizerSettings) -> Vec<String> {
    if !vis.enabled {
        return Vec::new();
    }
    vis.kinds
        .iter()
        .filter(|(_, on)| **on)
        .map(|(k, _)| k.clone())
        .collect()
}

/// State mutated by the socket handler (main thread), IPC commands and the
/// input thread: engine for `on_key`/`update_config`, store for saves.
pub struct Shared {
    pub engine: Engine,
    pub store: Store,
    /// FIFO writer for visualizer key events (lazy; drops when no reader).
    pub visual: VisualBus,
    /// `clicky-overlay <kind>` child processes.
    pub overlays: Overlays,
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
    let shared = Arc::new(Mutex::new(Shared {
        engine,
        store,
        visual: VisualBus::new(),
        overlays: Overlays::default(),
    }));
    // Spawn overlays for any already-enabled kinds.
    {
        let mut sh = shared.lock();
        let vis = sh.store.cfg.visualizer.clone();
            sh.overlays.sync(&vis);
    }

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
    {
        let mut sh = boot.shared.lock();
        sh.overlays.kill_all();
        if let Err(e) = sh.store.flush() {
            eprintln!("clicky: config save: {e}");
        }
    }
}

/// Create the socket dir (locked down 0700) and bind `control.sock`.
/// A connectable socket means a live instance owns it → `Err("busy")`, no
/// cleanup — callers must not remove a live peer's socket. `Err` strings are
/// human-readable; the caller prints and exits.
pub fn bind_socket() -> Result<UnixListener, String> {
    let sock_dir = socket_dir();
    std::fs::create_dir_all(&sock_dir).map_err(|e| format!("{}: {e}", sock_dir.display()))?;
    // The /tmp fallback dir would land at umask-0755 — lock it down so no
    // other user can pre-bind the socket, and refuse a foreign-owned dir.
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = std::fs::metadata(&sock_dir).map_err(|e| format!("{}: {e}", sock_dir.display()))?;
    if meta.uid() != unsafe { libc::getuid() } {
        return Err(format!("{} not owned by us", sock_dir.display()));
    }
    let mut perms = meta.permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(&sock_dir, perms)
        .map_err(|e| format!("{}: {e}", sock_dir.display()))?;
    let sock = socket_path();
    if UnixStream::connect(&sock).is_ok() {
        return Err(format!("busy: another instance owns {}", sock.display()));
    }
    // Stale socket file — safe to reclaim.
    let _ = std::fs::remove_file(&sock);
    let listener =
        UnixListener::bind(&sock).map_err(|e| format!("bind {}: {e}", sock.display()))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("socket nonblocking: {e}"))?;
    eprintln!("clicky: control socket on {}", sock.display());
    Ok(listener)
}

/// Send one command line to the instance owning the control socket; returns
/// the reply line.
pub fn send(line: &str) -> Result<String, String> {
    let sock = socket_path();
    let mut stream = UnixStream::connect(&sock)
        .map_err(|e| format!("can't reach daemon at {}: {e}", sock.display()))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    stream
        .write_all(format!("{line}\n").as_bytes())
        .map_err(|e| format!("send: {e}"))?;
    let mut reply = String::new();
    match BufReader::new(&stream).read_line(&mut reply) {
        Ok(0) => Err("closed without reply".into()),
        Ok(_) => Ok(reply.trim_end().to_string()),
        Err(e) => Err(format!("reply: {e}")),
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
    // Control socket — a connectable socket means a second instance: exit
    // rather than double-consume evdev.
    let listener = match bind_socket() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("clicky: {e}");
            shutdown(&mut b);
            return 1;
        }
    };

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
        // Reap/respawn overlay children each iteration (20 ms cadence).
        {
            let mut sh = b.shared.lock();
            let vis = sh.store.cfg.visualizer.clone();
            sh.overlays.sync(&vis);
        }
    }

    shutdown(&mut b);
    let _ = std::fs::remove_file(socket_path());
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
                        let mut sh = shared.lock();
                        sh.engine.on_key(&kev);
                        // Key IDs only, never typed text; drops silently
                        // when no overlay is reading the FIFO.
                        if sh.store.cfg.visualizer.any_enabled() {
                            let kinds = enabled_kinds(&sh.store.cfg.visualizer);
                            sh.visual.emit(&kev, &kinds);
                        }
                    }
                }
                InputEvent::Dropped { device } => {
                    normalizer.clear_held(device);
                    // Held keys vanished without release events — tell
                    // overlays to unpress everything so nothing stays lit.
                    let mut sh = shared.lock();
                    if sh.store.cfg.visualizer.any_enabled() {
                        let kinds = enabled_kinds(&sh.store.cfg.visualizer);
                        sh.visual.reset(&kinds);
                    }
                }
            },
            stop,
        );
        alive.store(false, Ordering::Relaxed);
        if let Err(e) = result {
            eprintln!("clicky: input loop failed: {e}");
        }
    })
}

/// Snapshot of the audio side `status` needs — callers gather it before
/// locking `shared` so a UI-side `Mutex<Audio>` never nests audio→shared.
pub struct AudioStatus {
    pub device_name: String,
    pub stats: clicky_core::mixer::Stats,
}

impl AudioStatus {
    pub fn of(audio: &Audio) -> Self {
        Self {
            device_name: audio.device_name().to_string(),
            stats: audio.stats(),
        }
    }
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
        Ok(_) => command(
            line.trim(),
            shared,
            controls,
            &AudioStatus::of(audio),
            quit,
            capture_alive,
        ),
    };
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.write_all(b"\n");
}

/// Dispatch one command; returns the reply line (no newline). Crate-public:
/// `ipc.rs`'s socket thread routes CLI verbs (enable/disable/profile/status)
/// here so the same control surface works against the UI instance.
pub(crate) fn command(
    line: &str,
    shared: &Arc<Mutex<Shared>>,
    controls: &MixerControls,
    audio: &AudioStatus,
    quit: &Arc<AtomicBool>,
    capture_alive: &Arc<AtomicBool>,
) -> String {
    let mut words = line.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some("status"), None, None) => {
            let mut sh = shared.lock();
            let vis = sh.store.cfg.visualizer.clone();
            sh.overlays.sync(&vis);
            status_line(&sh, audio, capture_alive)
        }
        // UI-mode only; the daemon is headless — still counts as a live
        // instance so the caller exits instead of double-booting.
        (Some("show"), None, None) => "err daemon mode".to_string(),
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
            if !sh.engine.profiles().iter().any(|p| p.id == *id) {
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

/// `status` reply: one JSON line. Caller holds the `shared` lock and has
/// already synced overlays so the map is fresh.
fn status_line(
    sh: &Shared,
    audio: &AudioStatus,
    alive: &Arc<AtomicBool>,
) -> String {
    // Enumerate readable nodes for counts; the reader's own liveness flag
    // decides "live" vs "dead" so a crashed loop never lies.
    let scan = input::keyboard_devices();
    let alive = alive.load(Ordering::Relaxed);
    serde_json::json!({
        "enabled": sh.store.cfg.enabled,
        "profile": sh.engine.profile_id(),
        "audio_device": audio.device_name,
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
        "overlays": sh.overlays.status_map(),
        "accepted": audio.stats.accepted,
        "dropped": audio.stats.dropped,
        "stolen": audio.stats.stolen,
    })
    .to_string()
}
