# AGENTS.md — clicky-linux

System-wide mechanical-keyboard sound engine for Wayland (Fedora/niri):
read-only evdev capture → normalizer → engine → lock-free mixer → cpal/PipeWire.
Tauri + React settings app; `clicky-overlay` renders layer-shell visualizers.
Port of the macOS "Clicky" schema; bundled banks are CC0-synthesized, not rips.

## Layout

- `crates/clicky-core` — engine lib: `keymap`, `input`, `normalizer`, `engine`,
  `mixer`, `audio`, `profiles`, `config`, `visual`, `diagnostics`, `import`
- `crates/clicky` — daemon/settings binary (CLI verbs + IPC socket) and probe
  bins `src/bin/{input_probe,audio_probe}.rs`
- `crates/clicky-overlay` — gtk4-layer-shell visualizer subprocess
- `ui/` — React + Vite settings UI (pnpm)
- `sounds/` — profile banks + `profiles.json` manifest + `LICENSES/`
- `tools/` — `setup.sh` (dnf deps, sudo), `sysroot.sh` (local -devel RPMs, no
  sudo), `install.sh`, `package.sh` (RPM), `gen_sounds.py`
- `udev/`, `packaging/` — uaccess rule, `.desktop` + icon
- `docs/` — `ARCHITECTURE.md` (authoritative design), `AUDIO_ENGINE.md`,
  `TESTING.md` (manual release-gate matrix)

## Commands (Makefile wraps these)

```
make build / release / test / fmt / clippy
make run | daemon | diagnostics        # cargo run -p clicky [-- --daemon|--diagnostics]
make ui / dev-ui                       # pnpm --dir ui {install+build|dev}
make install / install-udev / uninstall / package / setup / sysroot / sounds
```

CLI: `clicky` (app), `--daemon`, `--diagnostics`, `enable|disable|status|quit`,
`profile <id>`, `--input-probe`, `--audio-probe`.

## Setup (Fedora 44)

1. `bash tools/sysroot.sh` — populates `.sysroot/` with gtk/alsa/udev -devel
   files; `.cargo/config.toml` already points `PKG_CONFIG_PATH` at it. Needed
   to build the Tauri/overlay crates without sudo.
2. `make setup` / `tools/setup.sh --install` — dnf deps + udev rule (sudo).
3. udev uaccess rule is the only root step: `sudo cp udev/99-clicky-uaccess.rules
   /etc/udev/rules.d/` + reload, then log out/in or replug.

`mise install` provides rust/node/pnpm (tracks: stable/22/9).

## Hard invariants — do not break

- **No `EVIOCGRAB`, no input injection, no key-text capture or storage.**
  Capture is read-only evdev; sounds follow the physical device.
- **No allocation or locking in the mixer render callback** (`mixer.render`).
  Triggers arrive fully resolved via an rtrb SPSC ring (cap 1024); samples are
  pre-registered. All decode/resample happens off the RT path.
- **Key identity is HID `"page:usage"`** ("7:44" = Space); modifiers are
  page-7 usages 224–231, L/R distinct; mouse buttons page 9, usages 1–3.
  Same format as upstream Clicky → configs/overrides stay cross-compatible.
- **Profile switch = atomic swap**; release variant is chosen at press time
  and stored per-key so mid-hold switches can't cross wires.
- Gain chain: `slider × manifest × normalization × variation × 2`; recorded
  releases ×0.7; soft modifiers ×0.25.
- Config writes are atomic with 250 ms debounce
  (`~/.config/clicky/config.json`); don't write partial files.

## Testing

- `make test` — unit tests live in `crates/clicky-core` (`#[cfg(test)]`
  modules; mixer/keymap/normalizer/engine coverage).
- Release gate is the **manual matrix in `docs/TESTING.md`** — capture
  correctness, hotplug, permissions degradation, latency (<10 ms key→audio,
  ~3.3 ms measured), visualizers, packaging. RT/audio/evdev behavior can't be
  fully covered by unit tests; run the matrix for changes touching those paths.

## Conventions

- Rust: edition 2021, 4-space, `cargo fmt` + `clippy -D warnings` clean.
  Errors: `thiserror` in libs, `anyhow` at binary edges.
- UI: TypeScript/React, 2-space, pnpm only (lockfile + `packageManager`
  pinned; don't commit npm/yarn lockfiles).
- Shell: `tools/*.sh` are bash (setup/sysroot) or POSIX `sh` (install/package)
  with `set -eu`; keep them runnable from fish.
- Docs: update `docs/ARCHITECTURE.md` when module map, threads, or contracts
  change; update `docs/TESTING.md` when user-visible behavior changes.
