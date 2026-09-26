# Clicky for Linux

A native, **offline** mechanical-keyboard sound engine for Fedora 44 + Wayland +
niri. Every keystroke plays a satisfying switch sound in **any** application —
offline, privacy-first (key events never leave the machine; no typed text is
ever captured or stored, only raw key IDs flow through the pipeline).

Linux-native reimplementation of [Clicky](https://github.com/longmba/clicky)
(macOS, MIT). Inspired by Keeb/Thock.

## Install & run

See **[INSTALL.md](INSTALL.md)** — RPM or `install.sh`, plus the one `udevadm
trigger` step that enables keyboard capture.

```fish
clicky --daemon   # engine  →  type anywhere, sounds play
clicky            # settings window (single-instance)
```


## Development

`mise install` provisions the toolchain (rust stable, node 22, pnpm 9). GTK
and other `-devel` deps build against a local sysroot — no sudo:

```fish
bash tools/sysroot.sh   # populates .sysroot/ (auto-wired via .cargo/config.toml)
make build              # cargo build --workspace
make test               # unit tests (clicky-core)
make ui                 # pnpm install + build the settings UI
make run | daemon       # run the app / headless engine from target/debug
```

`make help` lists everything (fmt, clippy, diagnostics, install, package…).
Release gate is the manual matrix in [docs/TESTING.md](docs/TESTING.md).
Repo conventions and invariants for contributors/agents: [AGENTS.md](AGENTS.md).

## Docs
| File | What |
|---|---|
| [INSTALL.md](INSTALL.md) | Install, the `udevadm trigger` step, troubleshooting silence |
| [docs/PRODUCT_SPEC.md](docs/PRODUCT_SPEC.md) | Requirements & feature set |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Pipeline, crates, layer-shell overlays, deps |
| [docs/AUDIO_ENGINE.md](docs/AUDIO_ENGINE.md) | Mixer internals + measured ~3.3 ms latency |
| [docs/WAYLAND_INPUT.md](docs/WAYLAND_INPUT.md) | evdev capture, udev uaccess permission model |
| [docs/TESTING.md](docs/TESTING.md) | Fedora44+niri manual test matrix |
| [docs/IMPLEMENTATION_PLAN.md](docs/IMPLEMENTATION_PLAN.md) | Build order |

## Features

- System-wide sounds in every app (terminal, browser, Electron, Wayland-native)
- 20 bundled profiles (10 recorded MIT, 10 synthesized CC0) + user pack import
- Press/release samples, soft modifiers, per-key overrides, tone/pitch/pan
- Tauri 2 settings UI + headless `--daemon` + CLI (`status|enable|disable|profile|quit`)
- 5 layer-shell visualizer overlays incl. a draggable 3D keyboard
- RPM packaging, autostart, ~3.3 ms audio path on PipeWire
