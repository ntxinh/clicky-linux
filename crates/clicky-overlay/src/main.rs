//! clicky-overlay — gtk4-layer-shell visualizer subprocess.
//!
//! Spawned by the daemon as `clicky-overlay <kind>`; consumes the event
//! FIFO at $XDG_RUNTIME_DIR/clicky/events (`"{keyid} {phase}"` lines — key
//! IDs only, never typed text). Never in the audio path.
//!
//! Kinds: `keyboard` (15-col board, press → amber), `keystrokes` (pill
//! stack of key NAMES), `combo` (held-modifier symbols + non-mod count),
//! `bezel` (fullscreen pulsing edge border). All windows are layer
//! `overlay`, keyboard-mode none, click-through via an empty input region.

use std::cell::RefCell;
use std::collections::HashMap;
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4_layer_shell as layer_shell;
use gtk::glib;
use gtk::prelude::*;
use layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

/// Modifier usages (page 7): L/R Ctrl, Shift, Alt, GUI.
const MOD_USAGES: core::ops::RangeInclusive<u16> = 224..=231;
/// Held-modifier mask bit = usage − 224 (same encoding as the daemon).
fn mod_bit(usage: u16) -> u8 {
    (usage - 224) as u8
}
/// Mask bits: Ctrl = 224/228 → 0/4, Shift = 225/229 → 1/5,
/// Alt = 226/230 → 2/6, GUI = 227/231 → 3/7.
const CTRL_BITS: u8 = 0b0001_0001;
const SHIFT_BITS: u8 = 0b0010_0010;
const ALT_BITS: u8 = 0b0100_0100;
const GUI_BITS: u8 = 0b1000_1000;

/// Symbols in ⌃⌥⇧⌘ order.
fn mod_symbols(mask: u8) -> String {
    let mut s = String::new();
    if mask & CTRL_BITS != 0 {
        s.push('⌃');
    }
    if mask & ALT_BITS != 0 {
        s.push('⌥');
    }
    if mask & SHIFT_BITS != 0 {
        s.push('⇧');
    }
    if mask & GUI_BITS != 0 {
        s.push('⌘');
    }
    s
}

fn mod_symbol(usage: u16) -> &'static str {
    match usage {
        224 | 228 => "⌃",
        225 | 229 => "⇧",
        226 | 230 => "⌥",
        227 | 231 => "⌘",
        _ => "",
    }
}

/// Key NAME for pill labels — never typed text. Unknown → "·".
fn key_label(page: u8, usage: u16) -> String {
    match (page, usage) {
        (7, 4..=29) => return String::from(char::from(b'A' + (usage - 4) as u8)),
        (7, 30..=38) => return String::from(char::from(b'1' + (usage - 30) as u8)),
        (7, 39) => return "0".into(),
        (7, 40) | (7, 88) => "Enter",
        (7, 41) => "Esc",
        (7, 42) => "⌫",
        (7, 43) => "⇥",
        (7, 44) => "Space",
        (7, 45) => "-",
        (7, 46) => "=",
        (7, 47) => "[",
        (7, 48) => "]",
        (7, 49) => "\\",
        (7, 51) => ";",
        (7, 52) => "'",
        (7, 53) => "`",
        (7, 54) => ",",
        (7, 55) => ".",
        (7, 56) => "/",
        (7, 57) => "Caps",
        (7, 58..=69) => return format!("F{}", usage - 57),
        (7, 70) => "PrtSc",
        (7, 71) => "ScrLk",
        (7, 72) => "Pause",
        (7, 73) => "Ins",
        (7, 74) => "Home",
        (7, 75) => "PgUp",
        (7, 76) => "Del",
        (7, 77) => "End",
        (7, 78) => "PgDn",
        (7, 79) => "→",
        (7, 80) => "←",
        (7, 81) => "↓",
        (7, 82) => "↑",
        (7, u) if MOD_USAGES.contains(&u) => return mod_symbol(u).to_string(),
        (9, 1) => "LMB",
        (9, 2) => "RMB",
        (9, 3) => "MMB",
        _ => "·",
    }
    .into()
}

/// "7:44 press" → Some((7, 44, pressed)).
fn parse_line(line: &str) -> Option<(u8, u16, bool)> {
    let (id, phase) = line.trim().split_once(' ')?;
    let (p, u) = id.split_once(':')?;
    Some((p.parse().ok()?, u.parse().ok()?, phase == "press"))
}

/// ANSI 15-col board — engine.rs LAYOUT plus y = row (gap under F-row).
/// (usage, x, y, w)
const LAYOUT: &[(u16, f32, f32, f32)] = &[
    (41, 0.0, 0.0, 1.5), // esc
    (58, 1.5, 0.0, 1.0), (59, 2.5, 0.0, 1.0), (60, 3.5, 0.0, 1.0), (61, 4.5, 0.0, 1.0),
    (62, 5.5, 0.0, 1.0), (63, 6.5, 0.0, 1.0), (64, 7.5, 0.0, 1.0), (65, 8.5, 0.0, 1.0),
    (66, 9.5, 0.0, 1.0), (67, 10.5, 0.0, 1.0), (68, 11.5, 0.0, 1.0), (69, 12.5, 0.0, 1.0), // F1–F12
    (76, 13.5, 0.0, 1.5), // fwd delete
    (53, 0.0, 1.5, 1.0), (30, 1.0, 1.5, 1.0), (31, 2.0, 1.5, 1.0), (32, 3.0, 1.5, 1.0),
    (33, 4.0, 1.5, 1.0), (34, 5.0, 1.5, 1.0), (35, 6.0, 1.5, 1.0), (36, 7.0, 1.5, 1.0),
    (37, 8.0, 1.5, 1.0), (38, 9.0, 1.5, 1.0), (39, 10.0, 1.5, 1.0), (45, 11.0, 1.5, 1.0),
    (46, 12.0, 1.5, 1.0), // ` 1–0 − =
    (42, 13.0, 1.5, 2.0), // backspace
    (43, 0.0, 2.5, 1.5), // tab
    (20, 1.5, 2.5, 1.0), (26, 2.5, 2.5, 1.0), (8, 3.5, 2.5, 1.0), (21, 4.5, 2.5, 1.0),
    (23, 5.5, 2.5, 1.0), (28, 6.5, 2.5, 1.0), (24, 7.5, 2.5, 1.0), (12, 8.5, 2.5, 1.0),
    (18, 9.5, 2.5, 1.0), (19, 10.5, 2.5, 1.0), (47, 11.5, 2.5, 1.0), (48, 12.5, 2.5, 1.0), // Q–P [ ]
    (49, 13.5, 2.5, 1.5), // backslash
    (57, 0.0, 3.5, 1.75), // caps
    (4, 1.75, 3.5, 1.0), (22, 2.75, 3.5, 1.0), (7, 3.75, 3.5, 1.0), (9, 4.75, 3.5, 1.0),
    (10, 5.75, 3.5, 1.0), (11, 6.75, 3.5, 1.0), (13, 7.75, 3.5, 1.0), (14, 8.75, 3.5, 1.0),
    (15, 9.75, 3.5, 1.0), (51, 10.75, 3.5, 1.0), (52, 11.75, 3.5, 1.0), // A–L ; '
    (40, 12.75, 3.5, 2.25), // return
    (225, 0.0, 4.5, 2.25), // lshift
    (29, 2.25, 4.5, 1.0), (27, 3.25, 4.5, 1.0), (6, 4.25, 4.5, 1.0), (25, 5.25, 4.5, 1.0),
    (5, 6.25, 4.5, 1.0), (17, 7.25, 4.5, 1.0), (16, 8.25, 4.5, 1.0), (54, 9.25, 4.5, 1.0),
    (55, 10.25, 4.5, 1.0), (56, 11.25, 4.5, 1.0), // Z–M , . /
    (229, 12.25, 4.5, 2.75), // rshift
    (255, 0.0, 5.5, 1.0), (224, 1.0, 5.5, 1.0), (226, 2.0, 5.5, 1.0), (227, 3.0, 5.5, 1.25),
    (44, 4.25, 5.5, 5.0), (231, 9.25, 5.5, 1.25), (230, 10.5, 5.5, 1.0), (80, 11.5, 5.5, 1.0),
    (81, 12.5, 5.5, 1.0), (82, 13.5, 5.5, 0.75), (79, 14.25, 5.5, 0.75), // fn ⌃ ⌥ ⌘ space ⌘ ⌥ ←↓↑→
];

const CSS: &str = r#"
window { background: transparent; }
.board { padding: 8px; border-radius: 14px; background: rgba(20, 20, 24, 0.72); }
.key {
    border-radius: 6px;
    background: rgba(255, 255, 255, 0.08);
    color: rgba(255, 255, 255, 0.75);
    font-size: 12px;
    transition: background 220ms ease-out, color 220ms ease-out;
}
.key.pressed { background: rgba(255, 176, 32, 0.95); color: #1a1200; transition: none; }
.pill {
    border-radius: 999px;
    padding: 6px 18px;
    margin: 4px;
    background: rgba(20, 20, 24, 0.82);
    color: #f5f5f5;
    font-size: 20px;
    font-weight: 600;
    transition: opacity 400ms ease-out;
}
.pill.fading { opacity: 0; }
.hidden-pill { opacity: 0; }
.bezel {
    border: 4px solid rgba(255, 176, 32, 0.0);
    border-radius: 18px;
    transition: border-color 500ms ease-out;
}
.bezel.pulse { border-color: rgba(255, 176, 32, 0.9); transition: none; }
"#;

/// FIFO reader: O_RDWR so the reader also holds a "writer" — a blocking
/// read never returns EOF when the daemon's writer closes; a restarted
/// daemon re-attaches cleanly. Creates the FIFO if the daemon hasn't yet.
fn open_fifo(path: &Path) -> io::Result<File> {
    if let Err(e) = clicky_core::visual::ensure_fifo(path) {
        eprintln!("clicky-overlay: fifo {}: {e}", path.display());
    }
    OpenOptions::new().read(true).write(true).open(path)
}

/// Blocking reader thread → mpsc → main-context drain. (glib 0.22 dropped
/// `unix_fd_add`; a 10 ms poll is plenty for key events.) `on` runs on the
/// GTK thread once per parsed `(page, usage, pressed)`.
fn watch_fifo(file: File, on: impl Fn(u8, u16, bool) + 'static) {
    let (tx, rx) = std::sync::mpsc::channel::<(u8, u16, bool)>();
    std::thread::spawn(move || {
        let mut file = file;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match file.read(&mut chunk) {
                Ok(0) => return,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    while let Some(i) = buf.iter().position(|&c| c == b'\n') {
                        let line = String::from_utf8_lossy(&buf[..i]).into_owned();
                        buf.drain(..=i);
                        if let Some(ev) = parse_line(&line) {
                            if tx.send(ev).is_err() {
                                return;
                            }
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    });
    glib::timeout_add_local(Duration::from_millis(10), move || {
        while let Ok((p, u, press)) = rx.try_recv() {
            on(p, u, press);
        }
        glib::ControlFlow::Continue
    });
}

/// One layer-shell window: overlay layer, no keyboard focus, click-through.
fn layer_window(app: &gtk::Application, kind: &str) -> gtk::ApplicationWindow {
    let win = gtk::ApplicationWindow::new(app);
    win.set_decorated(false);
    win.init_layer_shell();
    win.set_layer(Layer::Overlay);
    win.set_keyboard_mode(KeyboardMode::None);
    let ns = format!("clicky:{kind}");
    win.set_namespace(Some(ns.as_str()));
    // Click-through: empty input region. GTK resets the region whenever the
    // surface lays out, so re-apply on its `layout` signal.
    win.connect_map(|w| {
        if let Some(surface) = w.surface() {
            surface.set_input_region(Some(&gtk::cairo::Region::create()));
            surface.connect_layout(|s, _, _| {
                s.set_input_region(Some(&gtk::cairo::Region::create()));
            });
        }
    });
    win
}

fn anchor(win: &gtk::ApplicationWindow, edges: &[Edge], margin: i32) {
    for &e in edges {
        win.set_anchor(e, true);
        if margin != 0 {
            win.set_margin(e, margin);
        }
    }
}


fn fifo_path() -> std::path::PathBuf {
    env::var("CLICKY_EVENTS_FIFO")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| clicky_core::visual::events_fifo())
}

fn fifo_or_exit() -> File {
    match open_fifo(&fifo_path()) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("clicky-overlay: fifo: {e}");
            std::process::exit(1);
        }
    }
}

/// keyboard: 15-col board, press → amber `.pressed`, release → CSS fade.
fn build_keyboard(app: &gtk::Application) -> gtk::ApplicationWindow {
    const CELL: f64 = 42.0;
    const GAP: f64 = 4.0;
    let win = layer_window(app, "keyboard");
    let fixed = gtk::Fixed::new();
    fixed.add_css_class("board");
    let step = CELL + GAP;
    let mut keys = HashMap::new();
    for &(usage, x, y, w) in LAYOUT {
        let name = key_label(7, usage);
        let key = gtk::Label::new(Some(name.as_str()));
        key.add_css_class("key");
        key.set_size_request((w as f64 * step - GAP) as i32, CELL as i32);
        fixed.put(&key, x as f64 * step, y as f64 * step);
        keys.insert(usage, key);
    }
    fixed.set_size_request((15.0 * step) as i32, (6.5 * step) as i32);
    win.set_child(Some(&fixed));
    anchor(&win, &[Edge::Bottom], 24);

    let keys = Rc::new(keys);
    watch_fifo(fifo_or_exit(), move |_p, u, press| {
        if let Some(k) = keys.get(&u) {
            if press {
                k.add_css_class("pressed");
            } else {
                k.remove_css_class("pressed");
            }
        }
    });
    win
}

/// keystrokes: bottom-center pill stack, "⇧K"-style NAME labels; pills
/// fade then drop.
fn build_keystrokes(app: &gtk::Application) -> gtk::ApplicationWindow {
    const MAX_PILLS: usize = 5;
    const HOLD_MS: u64 = 1400;
    let win = layer_window(app, "keystrokes");
    let stack = gtk::Box::new(gtk::Orientation::Vertical, 0);
    stack.set_halign(gtk::Align::Center);
    win.set_child(Some(&stack));
    anchor(&win, &[Edge::Bottom], 120);

    let held = Rc::new(RefCell::new(0u8));
    let stack = Rc::new(stack);
    watch_fifo(fifo_or_exit(), move |p, u, press| {
        let mut mask = held.borrow_mut();
        if MOD_USAGES.contains(&u) {
            if press {
                *mask |= mod_bit(u);
            } else {
                *mask &= !mod_bit(u);
            }
        }
        if !press {
            return;
        }
        // Mod presses contribute their symbol via the mask; a non-mod key
        // gets held-mod symbols + its NAME.
        let mut text = mod_symbols(*mask);
        if !MOD_USAGES.contains(&u) || text.is_empty() {
            text.push_str(&key_label(p, u));
        }
        drop(mask);

        let pill = gtk::Label::new(Some(text.as_str()));
        pill.add_css_class("pill");
        stack.append(&pill);
        while stack.observe_children().n_items() as usize > MAX_PILLS {
            match stack.first_child() {
                Some(first) => stack.remove(&first),
                None => break,
            }
        }
        // Fade after a hold, then remove once transparent.
        let pill2 = pill.clone();
        let stack2 = stack.clone();
        glib::timeout_add_local_once(Duration::from_millis(HOLD_MS), move || {
            pill2.add_css_class("fading");
            let stack3 = stack2.clone();
            let pill3 = pill2.clone();
            glib::timeout_add_local_once(Duration::from_millis(450), move || {
                stack3.remove(&pill3);
            });
        });
    });
    win
}

/// combo: top-center pill — held modifier symbols + count of non-mod
/// presses; resets when modifiers release or after `CLICKY_COMBO_TIMEOUT`
/// seconds (default 3).
fn build_combo(app: &gtk::Application) -> gtk::ApplicationWindow {
    let timeout = env::var("CLICKY_COMBO_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(3.0)
        .clamp(0.3, 30.0);
    let win = layer_window(app, "combo");
    let pill = gtk::Label::new(None);
    pill.add_css_class("pill");
    pill.add_css_class("hidden-pill");
    win.set_child(Some(&pill));
    anchor(&win, &[Edge::Top], 60);

    struct Combo {
        held: u8,
        count: u32,
        timer: Option<glib::SourceId>,
    }
    let state = Rc::new(RefCell::new(Combo {
        held: 0,
        count: 0,
        timer: None,
    }));
    let pill = Rc::new(pill);
    let render = {
        let state = state.clone();
        let pill = pill.clone();
        Rc::new(move || {
            let st = state.borrow();
            if st.held == 0 && st.count == 0 {
                pill.add_css_class("hidden-pill");
            } else {
                pill.remove_css_class("hidden-pill");
                let mut t = mod_symbols(st.held);
                if st.count > 0 {
                    if !t.is_empty() {
                        t.push(' ');
                    }
                    t.push_str(&format!("×{}", st.count));
                }
                pill.set_text(&t);
            }
        })
    };
    watch_fifo(fifo_or_exit(), move |_p, u, press| {
        {
            let mut st = state.borrow_mut();
            if MOD_USAGES.contains(&u) {
                if press {
                    st.held |= mod_bit(u);
                } else {
                    st.held &= !mod_bit(u);
                }
                st.count = 0;
            } else if press && st.held != 0 {
                st.count += 1;
            }
            if let Some(id) = st.timer.take() {
                id.remove();
            }
            let state2 = state.clone();
            let render2 = render.clone();
            st.timer = Some(glib::timeout_add_local_once(
                Duration::from_secs_f64(timeout),
                move || {
                    let mut st = state2.borrow_mut();
                    st.held = 0;
                    st.count = 0;
                    st.timer = None;
                    drop(st);
                    render2();
                },
            ));
        }
        render();
    });
    win
}

/// bezel: fullscreen edge frame that pulses on every key press.
fn build_bezel(app: &gtk::Application) -> gtk::ApplicationWindow {
    let win = layer_window(app, "bezel");
    let frame = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    frame.add_css_class("bezel");
    frame.set_hexpand(true);
    frame.set_vexpand(true);
    win.set_child(Some(&frame));
    anchor(&win, &[Edge::Top, Edge::Bottom, Edge::Left, Edge::Right], 0);

    let frame = Rc::new(frame);
    let timer = Rc::new(RefCell::new(None::<glib::SourceId>));
    watch_fifo(fifo_or_exit(), move |_p, _u, press| {
        if !press {
            return;
        }
        if let Some(id) = timer.borrow_mut().take() {
            id.remove();
        }
        frame.add_css_class("pulse");
        let frame2 = frame.clone();
        *timer.borrow_mut() = Some(glib::timeout_add_local_once(
            Duration::from_millis(120),
            move || frame2.remove_css_class("pulse"),
        ));
    });
    win
}

fn main() -> glib::ExitCode {
    let kind = env::args().nth(1).unwrap_or_default();
    if !matches!(kind.as_str(), "keyboard" | "keystrokes" | "combo" | "bezel") {
        eprintln!("usage: clicky-overlay <keyboard|keystrokes|combo|bezel>");
        std::process::exit(2);
    }

    let app_id = format!("dev.clicky.overlay.{kind}");
    let app = gtk::Application::new(Some(app_id.as_str()), Default::default());
    app.connect_activate(move |app| {
        if !layer_shell::is_supported() {
            eprintln!("clicky-overlay: layer shell not supported on this session");
            std::process::exit(1);
        }
        let win = match kind.as_str() {
            "keyboard" => build_keyboard(app),
            "keystrokes" => build_keystrokes(app),
            "combo" => build_combo(app),
            _ => build_bezel(app),
        };
        let provider = gtk::CssProvider::new();
        provider.load_from_data(CSS);
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        win.present();
        eprintln!("clicky-overlay: {kind} surface up");
    });
    // run() would hand argv to GApplication, which would try to open
    // "<kind>" as a file — pass a cleaned argv instead.
    app.run_with_args(&["clicky-overlay"])
}
