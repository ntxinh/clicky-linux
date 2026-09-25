# Clicky Linux — Implementation Plan

Authoritative task list: `docs/superpowers/plans/2026-09-26-clicky-linux.md`
(Tasks 0–17, TDD steps, per-task file lists and interfaces).
Spec: `docs/superpowers/specs/2026-09-26-clicky-linux-design.md`.

## Milestones

| M | Goal | Tasks |
|---|---|---|
| 1 | **Foundation** — toolchain, workspace scaffold, docs skeleton, feasibility probes | 0 scaffold/docs; 1 evdev-under-niri probe (uaccess); 2 cpal latency probe |
| 2 | **Sound engine** — evdev → mixer → one MIT pack; press/release, master vol, enable/disable, config persist, tray. *Type anywhere, hear sounds.* | 3 keymap; 4 normalizer; 5 input/hotplug; 6 mixer; 7 profiles; 10 audio; 11 daemon+tray wire-up |
| 3 | **Library** — 20 profiles, grouping/search/preview, user import | 8 config store; 12 gen_sounds + 20 packs; 13 pack import; 14 UI (partial) |
| 4 | **Tuning** — tone/pitch/pan, per-key tuning, soft modifiers, six presets | 9 engine resolution; 14 UI (partial) |
| 5 | **Visualizers** — keyboard, keystroke, combo, bezel (layer-shell), 3D | 15 visual.rs + overlays; 16 3D keyboard |
| 6 | **Integration** — device picker, hotplug, autostart, packaging, niri hooks, docs sync | 17 |

## Gates

- **M1 is the feasibility gate**: evdev must read keys on niri under the
  uaccess rule AND cpal must hit target latency on PipeWire *before* UI
  work begins.
- **Task 11 manual gate**: `cargo run -- --daemon`, type in terminal/
  browser — hear clicks, toggle off → silent. Do not proceed until it works.
- M5 probe decides 3D keyboard mechanism (layer-shell pointer input vs
  in-app three.js view).

## Prerequisites

System deps are not installed in the dev environment by default — run
`tools/setup.sh --install` (dnf: webkit2gtk4.1-devel, gtk4-devel,
gtk4-layer-shell-devel, libappindicator-gtk3-devel, librsvg2-devel,
alsa-lib-devel, libudev-devel, openssl-devel, nodejs; plus pnpm and the
udev rule). Until then, tauri/gtk4/udev deps stay commented out in
`crates/*/Cargo.toml` and cpal builds with `default-features = false`.
