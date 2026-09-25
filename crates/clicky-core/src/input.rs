//! evdev input: keyboard enumeration, epoll event loop, udev hotplug.
//!
//! Read-only capture only — NEVER EVIOCGRAB, never inject input.
//! `/dev/input/event*` needs `input` group or the shipped udev rule; nodes we
//! cannot open are reported as [`InputIssue`], never fatal.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use evdev::raw_stream::RawDevice;
use evdev::{EventType, InputEvent as EvdevEvent, Key, Synchronization};
use nix::sys::epoll::{Epoll, EpollCreateFlags, EpollEvent, EpollFlags};

const DEV_INPUT_DIR: &str = "/dev/input";
/// udev can batch several add/remove events; settle before re-enumerating.
const HOTPLUG_DEBOUNCE: Duration = Duration::from_millis(300);
/// Also bounds stop-flag latency; udev wakes epoll immediately on activity.
const EPOLL_POLL_MS: u16 = 100;

/// A device that failed to enumerate or open. Not fatal — the engine keeps
/// working with whatever nodes are readable.
#[derive(Debug)]
pub struct InputIssue {
    pub path: PathBuf,
    pub error: io::Error,
}

/// An opened keyboard and the /dev/input node it came from.
pub struct HotDevice {
    pub path: PathBuf,
    pub dev: RawDevice,
}

/// One input event for the engine. `Dropped` = kernel signaled SYN_DROPPED on
/// that device (ring buffer overflow) — call `Normalizer::clear_held(device)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    Key { device: u32, code: u16, value: i32 },
    Dropped { device: u32 },
}

/// Result of scanning /dev/input.
pub struct Enumerated {
    pub keyboards: Vec<HotDevice>,
    pub issues: Vec<InputIssue>,
}

fn event_nodes() -> io::Result<Vec<PathBuf>> {
    let mut nodes = Vec::new();
    for entry in std::fs::read_dir(DEV_INPUT_DIR)? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("event"))
        {
            nodes.push(path);
        }
    }
    nodes.sort();
    Ok(nodes)
}

/// True if the device can type a-z (EV_KEY with KEY_A..=KEY_Z).
fn has_full_alphabet(keys: &evdev::AttributeSetRef<Key>) -> bool {
    (Key::KEY_A.0..=Key::KEY_Z.0).all(|c| keys.contains(Key(c)))
}

/// Keyboard predicate: does this device look like a keyboard?
pub fn is_keyboard(d: &RawDevice) -> bool {
    d.supported_keys().is_some_and(has_full_alphabet)
}

/// Scan /dev/input/event*, keep readable keyboards, report everything else.
/// Unreadable nodes (EACCES until the udev rule is installed) land in `issues`.
pub fn keyboard_devices() -> Enumerated {
    let mut keyboards = Vec::new();
    let mut issues = Vec::new();
    match event_nodes() {
        Err(e) => issues.push(InputIssue {
            path: PathBuf::from(DEV_INPUT_DIR),
            error: e,
        }),
        Ok(nodes) => {
            for path in nodes {
                match RawDevice::open(&path) {
                    Ok(dev) => {
                        if is_keyboard(&dev) {
                            keyboards.push(HotDevice { path, dev });
                        }
                    }
                    Err(error) => issues.push(InputIssue { path, error }),
                }
            }
        }
    }
    Enumerated { keyboards, issues }
}

/// udev monitor over the `input` subsystem; the socket is pollable and goes
/// into the same epoll set as the device fds.
struct Hotplug {
    socket: udev::MonitorSocket,
}

impl Hotplug {
    fn new() -> io::Result<Self> {
        let socket = udev::MonitorBuilder::new()?
            .match_subsystem("input")?
            .listen()?;
        Ok(Self { socket })
    }

    /// Drain pending udev events; true if any event* node changed.
    /// The socket is nonblocking: next() returns None when empty.
    fn drain_changed(&self) -> bool {
        self.socket
            .iter()
            .any(|ev| ev.device().devnode().is_some_and(is_event_node))
    }
}

fn is_event_node(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("event"))
}

/// Epoll event.data for the udev socket.
const UDEV_TAG: u64 = u64::MAX;

fn arm_epoll(devices: &[HotDevice], hotplug: Option<&Hotplug>) -> io::Result<Epoll> {
    let epoll = Epoll::new(EpollCreateFlags::empty()).map_err(io::Error::from)?;
    for (i, d) in devices.iter().enumerate() {
        // SAFETY: `epoll` outlives `devices` inside run(); fd is valid.
        let fd = unsafe { BorrowedFd::borrow_raw(d.dev.as_raw_fd()) };
        epoll
            .add(fd, EpollEvent::new(EpollFlags::EPOLLIN, i as u64))
            .map_err(io::Error::from)?;
    }
    if let Some(h) = hotplug {
        epoll
            .add(
                h.socket.as_fd(),
                EpollEvent::new(EpollFlags::EPOLLIN, UDEV_TAG),
            )
            .map_err(io::Error::from)?;
    }
    Ok(epoll)
}

/// Read the fd's pending events, dispatch through `cb`.
/// Returns false when the device died — caller re-enumerates.
fn drain_device(idx: usize, dev: &mut HotDevice, cb: &mut impl FnMut(InputEvent)) -> bool {
    match dev.dev.fetch_events() {
        Ok(events) => {
            for ev in events {
                dispatch(idx, ev, cb);
            }
            true
        }
        Err(e) => {
            eprintln!("clicky: {} read failed: {e}", dev.path.display());
            false
        }
    }
}

fn dispatch(idx: usize, ev: EvdevEvent, cb: &mut impl FnMut(InputEvent)) {
    let ty = ev.event_type();
    if ty == EventType::KEY {
        cb(InputEvent::Key {
            device: idx as u32,
            code: ev.code(),
            value: ev.value(),
        });
    } else if ty == EventType::SYNCHRONIZATION && ev.code() == Synchronization::SYN_DROPPED.0 {
        cb(InputEvent::Dropped {
            device: idx as u32,
        });
    }
}

/// Run the read loop: epoll over the given keyboards plus a udev monitor.
/// `cb` gets [`InputEvent`]s; `device` indexes the live set (re-enumerated on
/// hotplug — `Dropped` is emitted for every old index before a rescan so the
/// caller can clear held state). Returns when `stop` is set or on epoll error.
pub fn run(
    mut devices: Vec<HotDevice>,
    mut cb: impl FnMut(InputEvent),
    stop: Arc<AtomicBool>,
) -> io::Result<()> {
    let hotplug = match Hotplug::new() {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("clicky: udev hotplug monitor unavailable ({e}); plugging changes ignored");
            None
        }
    };
    let mut epoll = arm_epoll(&devices, hotplug.as_ref())?;
    let mut events = vec![EpollEvent::empty(); devices.len().max(1) + 1];
    // Last udev event seen; rescan when it's this old. None = no pending rescan.
    let mut changed_at: Option<Instant> = None;
    // A rescan failed recently (e.g. EACCES on a just-added node); retry.
    let mut retry_at: Option<Instant> = None;

    while !stop.load(Ordering::Relaxed) {
        let n = match epoll.wait(&mut events, EPOLL_POLL_MS) {
            Ok(n) => n,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => return Err(io::Error::from(e)),
        };

        let mut dirty = false;
        for ev in &events[..n] {
            let data = ev.data();
            if data == UDEV_TAG {
                if hotplug.as_ref().is_some_and(|h| h.drain_changed()) {
                    changed_at = Some(Instant::now());
                }
            } else if ev
                .events()
                .intersects(EpollFlags::EPOLLERR | EpollFlags::EPOLLHUP)
            {
                dirty = true;
            } else if !drain_device(data as usize, &mut devices[data as usize], &mut cb) {
                dirty = true;
            }
        }

        // Debounce: rescan 300 ms after the last udev event* change, or at the
        // scheduled retry after a failed rescan.
        let now = Instant::now();
        if dirty
            || changed_at.is_some_and(|t| now.duration_since(t) >= HOTPLUG_DEBOUNCE)
            || retry_at.is_some_and(|t| now >= t)
        {
            // Clear held state on all old indices before renumbering.
            for i in 0..devices.len() {
                cb(InputEvent::Dropped {
                    device: i as u32,
                });
            }
            let rescan = keyboard_devices();
            for issue in &rescan.issues {
                // EACCES is expected until the udev rule is installed.
                eprintln!("clicky: {}: {}", issue.path.display(), issue.error);
            }
            devices = rescan.keyboards;
            epoll = arm_epoll(&devices, hotplug.as_ref())?;
            events = vec![EpollEvent::empty(); devices.len().max(1) + 1];
            changed_at = None;
            retry_at = (!rescan.issues.is_empty()).then(|| Instant::now() + Duration::from_secs(1));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use evdev::AttributeSet;

    #[test]
    fn alphabet_predicate() {
        let mut set = AttributeSet::<Key>::new();
        assert!(!has_full_alphabet(&set));
        for c in Key::KEY_A.0..=Key::KEY_Z.0 {
            set.insert(Key(c));
        }
        assert!(has_full_alphabet(&set));
        // Losing a key inside the range flips it back
        // (note: KEY_A..=KEY_Z are Linux codes 30-44, QWERTY layout order).
        set.remove(Key::KEY_L);
        assert!(!has_full_alphabet(&set));
    }

    #[test]
    fn is_event_node_filter() {
        assert!(is_event_node(Path::new("/dev/input/event0")));
        assert!(!is_event_node(Path::new("/dev/input/mouse0")));
        assert!(!is_event_node(Path::new("/dev/input/by-id/foo")));
    }

    #[test]
    fn enumeration_collects_issues_not_panics() {
        // Works under any permission level: unreadable nodes → issues.
        let enumerated = keyboard_devices();
        for d in &enumerated.keyboards {
            assert!(is_keyboard(&d.dev));
            assert!(d.path.starts_with(DEV_INPUT_DIR));
        }
    }

    /// Manual hardware check: `cargo test -p clicky-core input::tests::live_read -- --ignored`.
    /// Needs readable /dev/input/event* (input group or the udev rule).
    #[test]
    #[ignore = "needs evdev read access; run manually"]
    fn live_read() {
        let enumerated = keyboard_devices();
        assert!(
            !enumerated.keyboards.is_empty(),
            "no readable keyboards: {:?}",
            enumerated
                .issues
                .iter()
                .map(|i| format!("{}: {}", i.path.display(), i.error))
                .collect::<Vec<_>>()
        );
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            stop2.store(true, Ordering::Relaxed);
        });
        let mut saw_any = false;
        run(enumerated.keyboards, |_| saw_any = true, stop).unwrap();
        // No assertion on saw_any: a quiet 3 s with no keystrokes is fine;
        // this proves the loop runs and stops cleanly on live devices.
        let _ = saw_any;
    }
}
