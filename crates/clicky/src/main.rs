//! clicky — daemon + Tauri settings shell.
//!
//! CLI: `clicky` (app) | `--daemon` (headless autostart) |
//! `--diagnostics` (devices, perms, measured latency) |
//! `clicky enable|disable|profile <name>` (scripting/DMS keybinds).

fn main() {
    eprintln!(
        "clicky — mechanical keyboard sound engine\n\
         \n\
         usage:\n\
         \x20 clicky                  launch settings app + daemon\n\
         \x20 clicky --daemon         run headless (autostart)\n\
         \x20 clicky --diagnostics    device list, permission check, measured latency\n\
         \x20 clicky enable|disable   toggle sounds on a running daemon\n\
         \x20 clicky profile <name>   switch sound profile\n"
    );
}
