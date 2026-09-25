//! input_probe — enumerate keyboards, report permissions, read events ~10 s.
//! Basis for `clicky --diagnostics`. Safe to run without the udev rule:
//! unreadable /dev/input/event* nodes are listed as EACCES, not fatal.

use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clicky_core::input::{keyboard_devices, run, InputEvent};

fn main() {
    let enumerated = keyboard_devices();

    println!("keyboards (readable):");
    for d in &enumerated.keyboards {
        println!(
            "  {}  {}",
            d.path.display(),
            d.dev.name().unwrap_or("<unnamed>")
        );
    }
    println!("input issues:");
    for i in &enumerated.issues {
        let note = match i.error.kind() {
            ErrorKind::PermissionDenied => " (EACCES — needs input group / udev rule)",
            _ => "",
        };
        println!("  {}: {}{}", i.path.display(), i.error, note);
    }

    if enumerated.keyboards.is_empty() {
        println!("no readable keyboards — nothing to poll; exiting.");
        return;
    }

    println!("reading events for ~10 s — type something…");
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(10));
        stop2.store(true, Ordering::Relaxed);
    });
    if let Err(e) = run(
        enumerated.keyboards,
        |ev| match ev {
            InputEvent::Key {
                device,
                code,
                value,
            } => {
                let kind = match value {
                    0 => "release",
                    1 => "press",
                    2 => "repeat",
                    v => return println!("dev{device} code={code} value={v}"),
                };
                println!("dev{device} {kind} code={code}");
            }
            InputEvent::Dropped { device } => {
                println!("dev{device} SYN_DROPPED (held state must be cleared)")
            }
        },
        stop,
    ) {
        eprintln!("input loop failed: {e}");
        std::process::exit(1);
    }
    println!("done.");
}
