//! clicky — daemon + CLI control.
//!
//! `clicky` / `clicky --daemon` runs the engine headless (T14 adds the
//! settings window). `clicky enable|disable|profile <id>|status|quit` talks
//! to a running daemon over its Unix control socket. `--diagnostics` prints
//! devices, evdev access and audio host/buffer info. `--input-probe` /
//! `--audio-probe` re-exec the dev probes shipped next to this binary.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::exit;

use clicky_core::audio::Audio;
use clicky_core::input;
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::BufferSize;

mod daemon;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        None | Some("--daemon") => daemon::run(),
        Some("--diagnostics") => diagnostics(),
        Some("--input-probe") => probe("input_probe"),
        Some("--audio-probe") => probe("audio_probe"),
        Some(cmd @ ("enable" | "disable" | "status" | "quit")) => client(cmd, &[]),
        Some("profile") => match args.get(1) {
            Some(id) => client("profile", &[id]),
            None => usage_err("profile needs an id"),
        },
        Some(other) => usage_err(&format!("unknown argument {other}")),
    };
    exit(code);
}

fn usage_err(msg: &str) -> i32 {
    eprintln!(
        "clicky: {msg}\n\
         \n\
         usage:\n\
         \x20 clicky                  run daemon (autostart/foreground)\n\
         \x20 clicky --daemon         same, explicit\n\
         \x20 clicky --diagnostics    device list, permission check, buffer sizes\n\
         \x20 clicky enable|disable   toggle sounds on the running daemon\n\
         \x20 clicky profile <id>     switch sound profile\n\
         \x20 clicky status           daemon state as JSON\n\
         \x20 clicky quit             stop the daemon\n\
         \x20 clicky --input-probe    evdev read probe (dev)\n\
         \x20 clicky --audio-probe    playback latency probe (dev)"
    );
    2
}

/// Send one command line to the running daemon; print the reply line.
fn client(cmd: &str, rest: &[&String]) -> i32 {
    let sock = daemon::socket_path();
    let mut stream = match UnixStream::connect(&sock) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("clicky: can't reach daemon at {}: {e}", sock.display());
            return 1;
        }
    };
    let mut line = cmd.to_string();
    for a in rest {
        line.push(' ');
        line.push_str(a);
    }
    line.push('\n');
    if let Err(e) = stream.write_all(line.as_bytes()) {
        eprintln!("clicky: send: {e}");
        return 1;
    }
    let mut reply = String::new();
    match BufReader::new(&stream).read_line(&mut reply) {
        Ok(0) => {
            eprintln!("clicky: daemon closed without reply");
            1
        }
        Ok(_) => {
            let reply = reply.trim_end();
            println!("{reply}");
            if reply.starts_with("err") {
                1
            } else {
                0
            }
        }
        Err(e) => {
            eprintln!("clicky: reply: {e}");
            1
        }
    }
}

/// Re-exec a dev probe binary installed next to this one.
fn probe(name: &str) -> i32 {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(name)));
    match exe {
        Some(p) if p.exists() => {
            let status = std::process::Command::new(&p).status();
            match status {
                Ok(s) => s.code().unwrap_or(1),
                Err(e) => {
                    eprintln!("clicky: {}: {e}", p.display());
                    1
                }
            }
        }
        _ => {
            eprintln!("clicky: {name} not next to this binary — `cargo run --bin {name}`");
            1
        }
    }
}

/// `--diagnostics`: readable keyboards, per-node access issues, audio host,
/// output devices, negotiated-config guess and the buffer-size ladder.
fn diagnostics() -> i32 {
    println!("input:");
    let enumerated = input::keyboard_devices();
    if enumerated.keyboards.is_empty() {
        println!("  (no readable keyboards)");
    }
    for d in &enumerated.keyboards {
        println!(
            "  {}  {}",
            d.path.display(),
            d.dev.name().unwrap_or("<unnamed>")
        );
    }
    for i in &enumerated.issues {
        let note = match i.error.kind() {
            std::io::ErrorKind::PermissionDenied => " — EACCES, needs input group / udev rule",
            _ => "",
        };
        println!("  ! {}: {}{}", i.path.display(), i.error, note);
    }

    println!("audio:");
    let host = cpal::default_host();
    println!("  host: {:?}", host.id());
    if let Some(dev) = host.default_output_device() {
        let name = dev.name().unwrap_or_else(|_| "<unnamed>".into());
        match dev.default_output_config() {
            Ok(c) => println!(
                "  default: {name}  {} Hz {} ch {:?}",
                c.sample_rate().0,
                c.channels(),
                c.sample_format()
            ),
            Err(e) => println!("  default: {name}  (config: {e})"),
        }
    } else {
        println!("  default: none");
    }
    for d in Audio::devices() {
        println!(
            "  out: {}{}",
            d.name,
            if d.is_default { " (default)" } else { "" }
        );
    }
    println!("  buffer attempts:");
    for b in Audio::buffer_attempts() {
        match b {
            BufferSize::Fixed(n) => println!("    fixed {n} frames"),
            BufferSize::Default => println!("    device default"),
        }
    }
    0
}
