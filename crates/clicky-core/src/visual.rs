//! Visualizer event bus — `{keyid} {press|release}` lines on per-kind
//! FIFOs at `$XDG_RUNTIME_DIR/clicky/events.<kind>`, consumed by
//! `clicky-overlay <kind>` subprocesses. Key IDs only — never typed text.
//! One FIFO per kind: a shared FIFO would split each byte across readers,
//! starving every overlay but one.
//!
//! FIFOs are created lazily on first emit; writers are nonblocking so a
//! missing or stalled reader drops the write and never stalls the audio
//! path. Callers gate on `visualizer.enabled` themselves.
//!
//! Protocol: `"7:44 press"` / `"7:44 release"` per event, plus `"* reset"`
//! — clear all held/pressed state (sent on SYN_DROPPED).

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use crate::normalizer::{KeyEvent, Phase};

/// `/run/user/<uid>/clicky` — control socket + event FIFOs live here.
pub fn runtime_dir() -> PathBuf {
    directories::BaseDirs::new()
        .and_then(|b| b.runtime_dir().map(|d| d.join("clicky")))
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/clicky-{}", unsafe { nix::libc::getuid() })))
}

/// `$XDG_RUNTIME_DIR/clicky/events.<kind>` — one FIFO per overlay kind.
pub fn kind_fifo(kind: &str) -> PathBuf {
    runtime_dir().join(format!("events.{kind}"))
}

/// Create the FIFO (0600) and its parent dir (0700) if absent.
pub fn ensure_fifo(path: &std::path::Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    match nix::unistd::mkfifo(path, nix::sys::stat::Mode::from_bits_truncate(0o600)) {
        Ok(()) | Err(nix::errno::Errno::EEXIST) => {}
        Err(e) => return Err(io::Error::from_raw_os_error(e as i32)),
    }
    let mut perms = fs::metadata(path)?.permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o600);
    fs::set_permissions(path, perms)
}

/// Nonblocking writer set — one lazily-opened FIFO per overlay kind.
/// Cheap enough to construct unused; FIFOs open on first emit.
#[derive(Debug, Default)]
pub struct VisualBus {
    dir: Option<PathBuf>,
    writers: HashMap<String, File>,
}

impl VisualBus {
    /// Bus on the standard `$XDG_RUNTIME_DIR/clicky` dir.
    pub fn new() -> Self {
        Self {
            dir: Some(runtime_dir()),
            writers: HashMap::new(),
        }
    }

    /// Bus writing `events.<kind>` under an explicit dir (tests).
    pub fn at(dir: PathBuf) -> Self {
        Self {
            dir: Some(dir),
            writers: HashMap::new(),
        }
    }

    /// Lazily open the nonblocking writer for `kind`. `None` (drop) when no
    /// reader holds the FIFO (ENXIO) or creation/open fails.
    fn writer(&mut self, kind: &str) -> Option<&mut File> {
        if !self.writers.contains_key(kind) {
            let path = self.dir.as_ref()?.join(format!("events.{kind}"));
            if let Err(e) = ensure_fifo(&path) {
                eprintln!("clicky: visual fifo {}: {e}", path.display());
                return None;
            }
            match OpenOptions::new()
                .write(true)
                .custom_flags(nix::libc::O_NONBLOCK)
                .open(&path)
            {
                Ok(f) => {
                    self.writers.insert(kind.to_string(), f);
                }
                Err(e) => {
                    // ENXIO = no reader yet — expected, not a complaint.
                    if e.raw_os_error() != Some(nix::libc::ENXIO) {
                        eprintln!("clicky: visual fifo {}: {e}", path.display());
                    }
                    return None;
                }
            }
        }
        self.writers.get_mut(kind)
    }

    /// Write `line` to every enabled kind's FIFO. WouldBlock / EPIPE
    /// (reader left → writer reset, lazy reopen next event) drop silently.
    fn write_all_kinds(&mut self, line: &str, kinds: &[String]) {
        for kind in kinds {
            let Some(w) = self.writer(kind) else {
                continue;
            };
            match w.write_all(line.as_bytes()) {
                Err(e) if e.raw_os_error() == Some(nix::libc::EPIPE) => {
                    self.writers.remove(kind);
                }
                _ => {} // WouldBlock and transient errors → drop
            }
        }
    }

    /// Emit `"{keyid} {press|release}\n"` to each enabled kind's FIFO.
    pub fn emit(&mut self, ev: &KeyEvent, kinds: &[String]) {
        let phase = match ev.phase {
            Phase::Press => "press",
            Phase::Release => "release",
        };
        self.write_all_kinds(&format!("{} {phase}\n", ev.keyid), kinds);
    }

    /// Emit `"* reset"` — overlays clear all pressed/held state. Sent when
    /// the daemon's held-set is dropped (SYN_DROPPED).
    pub fn reset(&mut self, kinds: &[String]) {
        self.write_all_kinds("* reset\n", kinds);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("clicky-vis-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn ev(phase: Phase) -> KeyEvent {
        KeyEvent {
            device: 0,
            keyid: "7:44",
            usage: 44,
            page: 7,
            phase,
            modifiers: 0,
        }
    }

    fn open_reader(path: &std::path::Path) -> File {
        // O_RDWR keeps a "writer" attached so reads never hit EOF when the
        // real writer closes — mirroring clicky-overlay's reader.
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(nix::libc::O_NONBLOCK)
            .open(path)
            .unwrap()
    }

    fn drain(rd: &mut File, want: &str) -> String {
        let mut buf = String::new();
        for _ in 0..50 {
            let _ = rd.read_to_string(&mut buf);
            if buf.contains(want) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        buf
    }

    #[test]
    fn round_trip_per_kind() {
        let dir = tmpdir("rt");
        let kinds = vec!["keystrokes".to_string()];
        let fifo = dir.join("events.keystrokes");
        ensure_fifo(&fifo).unwrap();
        let mut rd = open_reader(&fifo);
        let mut bus = VisualBus::at(dir.clone());
        bus.emit(&ev(Phase::Press), &kinds);
        bus.emit(&ev(Phase::Release), &kinds);
        assert_eq!(drain(&mut rd, "release"), "7:44 press\n7:44 release\n");
        let mode = fs::metadata(&fifo).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_kind_gets_every_event() {
        let dir = tmpdir("multi");
        let kinds = vec!["keystrokes".to_string(), "combo".to_string()];
        let f1 = dir.join("events.keystrokes");
        let f2 = dir.join("events.combo");
        ensure_fifo(&f1).unwrap();
        ensure_fifo(&f2).unwrap();
        let mut r1 = open_reader(&f1);
        let mut r2 = open_reader(&f2);
        let mut bus = VisualBus::at(dir.clone());
        bus.emit(&ev(Phase::Press), &kinds);
        // BOTH readers get the press — the shared-FIFO bug this prevents.
        assert_eq!(drain(&mut r1, "press"), "7:44 press\n");
        assert_eq!(drain(&mut r2, "press"), "7:44 press\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reset_line() {
        let dir = tmpdir("reset");
        let kinds = vec!["keyboard".to_string()];
        ensure_fifo(&dir.join("events.keyboard")).unwrap();
        let mut rd = open_reader(&dir.join("events.keyboard"));
        let mut bus = VisualBus::at(dir.clone());
        bus.reset(&kinds);
        assert_eq!(drain(&mut rd, "reset"), "* reset\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_absent_drops_without_blocking() {
        let dir = tmpdir("no-reader");
        ensure_fifo(&dir.join("events.keystrokes")).unwrap();
        let mut bus = VisualBus::at(dir.clone());
        // No reader: emit must return immediately.
        bus.emit(&ev(Phase::Press), &["keystrokes".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_leaves_then_returns() {
        let dir = tmpdir("rejoin");
        let kinds = vec!["keystrokes".to_string()];
        let fifo = dir.join("events.keystrokes");
        ensure_fifo(&fifo).unwrap();
        let mut bus = VisualBus::at(dir.clone());
        {
            let _rd = open_reader(&fifo);
            bus.emit(&ev(Phase::Press), &kinds); // writer opened, delivered
        } // last reader gone — next write is EPIPE → dropped, writer reset
        bus.emit(&ev(Phase::Press), &kinds);
        let mut rd = open_reader(&fifo);
        bus.emit(&ev(Phase::Release), &kinds);
        assert_eq!(drain(&mut rd, "release"), "7:44 release\n");
        let _ = fs::remove_dir_all(&dir);
    }
}
