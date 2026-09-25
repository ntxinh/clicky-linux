//! clicky-overlay — gtk4-layer-shell visualizer subprocess.
//!
//! Spawned by the daemon as `clicky-overlay <kind>`; consumes the event
//! FIFO at $XDG_RUNTIME_DIR/clicky/events. Never in the audio path.
//!
//! gtk4/gtk4-layer-shell deps are commented out until tools/setup.sh runs.

fn main() {
    eprintln!(
        "clicky-overlay — visualizer subprocess\n\
         \n\
         usage: clicky-overlay <kind>\n\
         \x20 kinds: keyboard | keystrokes | combo | bezel | keyboard3d\n"
    );
}
