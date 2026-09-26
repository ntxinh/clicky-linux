//! Visualizer event bus — `{keyid} {press|release}` lines on a FIFO at
//! `$XDG_RUNTIME_DIR/clicky/events`, consumed by `clicky-overlay <kind>`
//! subprocesses. Key IDs only — never typed text.
//!
//! The FIFO is created lazily on first emit; the writer is nonblocking so a
//! missing or stalled reader drops the write and never stalls the audio
//! path. Callers gate on `visualizer.enabled` themselves.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use crate::normalizer::{KeyEvent, Phase};

/// `/run/user/<uid>/clicky` — control socket + events FIFO live here.
pub fn runtime_dir() -> PathBuf {
    directories::BaseDirs::new()
        .and_then(|b| b.runtime_dir().map(|d| d.join("clicky")))
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/clicky-{}", unsafe { nix::libc::getuid() })))
}

/// `$XDG_RUNTIME_DIR/clicky/events`.
pub fn events_fifo() -> PathBuf {
    runtime_dir().join("events")
}

/// Create the FIFO (0600) and its parent dir (0700) if absent.
pub fn ensure_fifo(path: &std::path::Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    match nix::unistd::mkfifo(
        path,
        nix::sys::stat::Mode::from_bits_truncate(0o600),
    ) {
        Ok(()) | Err(nix::errno::Errno::EEXIST) => {}
        Err(e) => return Err(io::Error::from_raw_os_error(e as i32)),
    }
    let mut perms = fs::metadata(path)?.permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o600);
    fs::set_permissions(path, perms)
}

/// Nonblocking FIFO writer. Cheap enough to construct unused — the FIFO is
/// only opened on first emit.
#[derive(Debug, Default)]
pub struct VisualBus {
    path: PathBuf,
    writer: Option<File>,
}

impl VisualBus {
    /// Bus on the standard `$XDG_RUNTIME_DIR/clicky/events` path.
    pub fn new() -> Self {
        Self {
            path: events_fifo(),
            writer: None,
        }
    }

    /// Bus on an explicit FIFO path (tests, preview tools).
    pub fn at(path: PathBuf) -> Self {
        Self { path, writer: None }
    }

    /// Lazily open the nonblocking writer. Returns `None` (drop this event)
    /// when no reader holds the FIFO (ENXIO) or creation/open fails.
    fn writer(&mut self) -> Option<&mut File> {
        if self.writer.is_none() {
            if let Err(e) = ensure_fifo(&self.path) {
                eprintln!("clicky: visual fifo {}: {e}", self.path.display());
                return None;
            }
            match OpenOptions::new()
                .write(true)
                .custom_flags(nix::libc::O_NONBLOCK)
                .open(&self.path)
            {
                Ok(f) => self.writer = Some(f),
                Err(e) => {
                    // ENXIO = no reader yet — expected, not a complaint.
                    if e.raw_os_error() != Some(nix::libc::ENXIO) {
                        eprintln!("clicky: visual fifo {}: {e}", self.path.display());
                    }
                    return None;
                }
            }
        }
        self.writer.as_mut()
    }

    /// Emit `"{keyid} {press|release}\n"`. WouldBlock / EPIPE (reader left)
    /// drop the event silently — the audio path never waits on overlays.
    pub fn emit(&mut self, ev: &KeyEvent) -> io::Result<()> {
        let phase = match ev.phase {
            Phase::Press => "press",
            Phase::Release => "release",
        };
        let Some(w) = self.writer() else {
            return Ok(());
        };
        match writeln!(w, "{} {phase}", ev.keyid) {
            Err(e) if e.raw_os_error() == Some(nix::libc::EPIPE) => {
                // Last reader went away — reopen lazily next event.
                self.writer = None;
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
            r => r,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;

    fn tmpfifo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("clicky-vis-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("events")
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

    #[test]
    fn round_trip() {
        let path = tmpfifo("rt");
        ensure_fifo(&path).unwrap();
        let mut rd = open_reader(&path);
        let mut bus = VisualBus::at(path.clone());
        bus.emit(&ev(Phase::Press)).unwrap();
        bus.emit(&ev(Phase::Release)).unwrap();

        let mut buf = String::new();
        for _ in 0..50 {
            let _ = rd.read_to_string(&mut buf);
            if buf.contains("release") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(buf, "7:44 press\n7:44 release\n");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reader_absent_drops_without_blocking() {
        let path = tmpfifo("no-reader");
        ensure_fifo(&path).unwrap();
        let mut bus = VisualBus::at(path.clone());
        // No reader: emit must return immediately with Ok.
        bus.emit(&ev(Phase::Press)).unwrap();
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reader_leaves_then_returns() {
        let path = tmpfifo("rejoin");
        ensure_fifo(&path).unwrap();
        let mut bus = VisualBus::at(path.clone());
        {
            let _rd = open_reader(&path);
            bus.emit(&ev(Phase::Press)).unwrap(); // writer opened, event delivered
        } // last reader gone — next write is EPIPE → dropped, writer reset
        bus.emit(&ev(Phase::Press)).unwrap();
        let mut rd = open_reader(&path);
        bus.emit(&ev(Phase::Release)).unwrap();
        let mut buf = String::new();
        for _ in 0..50 {
            let _ = rd.read_to_string(&mut buf);
            if buf.contains("release") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(buf, "7:44 release\n");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
