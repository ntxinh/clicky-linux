# Clicky Linux — Product Spec

Condensed from `docs/superpowers/specs/2026-09-26-clicky-linux-design.md` (authority).
Target: Fedora Workstation 44, Wayland, niri 26.04, DankMaterialShell, fish.

## Goal

Native offline mechanical-keyboard sound engine: plays switch sounds on
physical key press/release **in every application**, system-wide, under
Wayland — where per-app or X11-style hooks don't work.

## What it is

- One binary `clicky`: daemon + Tauri 2 settings shell in-process
  (React/TS UI); `clicky-overlay` subprocess renders optional visualizers.
- Input: `evdev` on `/dev/input/event*`, read-only, gated by a udev
  `uaccess` rule — no root daemon, no input group.
- Audio: cpal → PipeWire, custom lock-free mixer (Rust port of Clicky's
  CClickyAudio): 96 voices, per-key overrides, soft modifiers, tone/pitch/
  pan tuning.
- Sound library: 10 tplai MIT recorded packs + 10 fresh CC0 synthesized
  banks; user pack import (`pack.json`, WAV/MP3/FLAC/OGG).
- CLI: `clicky` (app), `--daemon`, `--diagnostics`, `enable|disable`,
  `profile <name>` — scriptable for DMS keybinds.

## Privacy contract

Offline always. Never: text reconstruction, keystroke storage/transmission,
telemetry, accounts, screenshots, mic. Only `EV_KEY` code + phase + device
id are processed. Never `EVIOCGRAB`, never inject input.

## Non-goals (v1)

- Head tracking (no Linux API — documented limitation; interface stays
  extensible)
- Mouse sounds (config schema supports them; ships later)
- DMS plugin (coexistence via tray only)
- Windows/macOS ports, GNOME Shell extensions, X11 support, root requirement
- Clicky's original 10 WAV banks (permission-bound to Clicky — not
  redistributable; CC0 replacements synthesized instead)

## Success criteria

- Type in any app (terminal, browser, Electron) → sounds, zero interference
  with the compositor's own input handling.
- Measured <10 ms key→audio, <1% CPU idle, <50 MB RSS headless
  (`--diagnostics` reports real numbers — never claimed).
- Permission denied → degraded mode: library/preview/customization still
  usable, engine idles.

## Milestones

See `docs/IMPLEMENTATION_PLAN.md` (M1 foundation/probes → M6 packaging).
