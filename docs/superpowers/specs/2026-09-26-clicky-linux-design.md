# Clicky Linux — Design Spec

2026-09-26 · Target: Fedora Workstation 44, Wayland, niri 26.04, DankMaterialShell, fish

Native offline mechanical keyboard sound engine. Plays switch sounds on physical
key press/release in every application. Privacy-defining: no text reconstruction,
no telemetry, no network.

Upstream references studied: `~/GitRepos/Code/clicky` (macOS SwiftUI/AppKit
original), `~/GitRepos/Code/thock` (Tauri 2 Linux attempt — studied for reusable
patterns and its Wayland failure mode).

## Locked decisions

| Decision | Choice |
|---|---|
| Process model | One binary `clicky`: daemon + Tauri shell in-process; `--overlay <kind>` subprocess for visualizers |
| UI stack | Tauri 2 + React + TypeScript |
| Input permission | udev `uaccess` rule (guided opt-in), NOT input group, NOT root daemon |
| Bundled audio | 10 tplai MIT packs (recorded, press+release) + 10 CC0 procedural banks |
| Audio stack | cpal → PipeWire; custom lock-free mixer (port of Clicky's CClickyAudio) |
| Input stack | `evdev` crate on `/dev/input/event*`; never rdev (X11-only on Linux — verified thock dead-end) |

## 1. Process & threading

Single binary. Launch = daemon: config → input thread → mixer → cpal stream →
Tauri (tray via `tray-icon`/SNI + hidden settings window). Window close → hide;
tray Quit → exit. Engine threads outlive the webview; daemon is headless when
the window is closed.

- **Input thread**: `evdev` + `epoll` across keyboard devices; udev monitor +
  `inotify` on `/dev/input` for hotplug. Emits `KeyEvent{usage, phase,
  device_id, timestamp}`.
- **Engine logic** (same thread): repeat suppression (evdev value==2), held-set
  tracking, modifier aggregation, override lookup → `Trigger` pushed to mixer
  SPSC queue (cap 1024, drop-oldest + overflow counter); mirrored to visualizer
  FIFO `$XDG_RUNTIME_DIR/clicky/events` only when a visualizer is enabled.
- **Audio render**: cpal stream callback owns the mixer. Zero alloc/lock in
  callback; triggers arrive fully resolved.
- **Overlay subprocess**: `clicky --overlay <kind>` — thin gtk4-layer-shell
  client consuming the event FIFO. Supervisor restarts once on crash, then marks
  the visualizer failed in UI. Never in the audio path.
- **CLI**: `clicky` (app), `--daemon` (headless autostart),
  `--overlay <kind>`, `--diagnostics` (device list, perm check, measured
  latency), `clicky enable|disable|profile <name>` (scripting/DMS keybinds).

## 2. Input subsystem

- `evdev` crate, read-only. Enumerate devices where `EV_KEY` is supported and
  `KEY_A..KEY_Z` bits present → keyboard; mice/tablets excluded. Re-enumerate on
  udev events; multiple keyboards supported, `device_id` keeps held-state
  per device.
- `EV_KEY` only: value 0=release, 1=press, 2=repeat → suppress (configurable).
  `SYN_DROPPED` → clear held-set and resync device state. **Never**
  `EVIOCGRAB` — kernel still delivers events to niri; zero interference, no
  injection.
- **Key identity**: evdev `KEY_*` → HID `"page:usage"` via static table
  (KEY_A=30→`"7:4"`, Enter=28→`"7:40"`, modifiers 224–231 L/R distinct).
  Adopts Clicky's identity verbatim → config v1 and key overrides are
  format-compatible. Page 9 (mouse buttons 1–3) and page 12 (consumer/media)
  supported as sound triggers; media keys get sounds but aren't in the 2D
  layout.
- **Permissions (uaccess)**: ship `99-clicky-uaccess.rules`:
  `SUBSYSTEM=="input", KERNEL=="event*", ENV{ID_INPUT_KEYBOARD}=="1",
  TAG+="uaccess"` — logind grants seat-user ACL on keyboard nodes only, active
  session only. UI detects `EACCES` → shows the guided one-time `pkexec`
  install command. Degraded mode: engine idles; library/preview/customization
  fully usable. Tradeoff documented: while installed, any same-user process
  can read keyboard events (true of all passive capture).
- No portal covers passive global capture (InputCapture = KVM-shaped,
  RemoteDesktop = injection-shaped, global-shortcuts = per-hotkey). Documented
  limitation in `docs/WAYLAND_INPUT.md`.

## 3. Audio engine

- **cpal** output stream on PipeWire; `StreamConfig` from device default;
  request small buffer (~128 frames where honored). Measured latency reported
  via `--diagnostics` — never claimed.
- All pack samples preloaded + decoded to mono f32 at load/profile switch
  (switch = atomic pointer swap → instant). No disk I/O or decode per press.
- **Mixer** = Rust port of `CClickyAudio.c`:
  - 96 voices, SPSC trigger queue cap 1024, max 2048 registered samples.
  - `Trigger{sample_idx, gain, pan, pitch, tone}`.
  - Linear-interp resample, step = `srcRate/outRate × pitch`.
  - One-pole tone: tone<0 → LP cutoff `18000×0.06^{-tone}`; tone>0 → brightness
    add `+80%(dry−filtered)`; 0 = bypass.
  - Constant-power pan, angle = `(pan+1)×π/4`.
  - Per-voice 0.5 ms attack / 2 ms end-release ramps; 3 ms slewed master
    gain/enable (no pops on toggle).
  - Soft limiter: transparent below −3.1 dBFS, knee to 0.9999 FS.
  - Oldest-voice steal when full; stats counters
    (rendered/accepted/dropped/stolen/active).
- **Gain stack** (from Clicky DSP.swift):
  `gain = sliderVolume × manifestGain × normalization × variation ×
  calibration(×2)`; typing path adds per-key override volume, modifier gain,
  home-row softness. Recorded releases ×0.7 (≈−3.1 dB).
- **Variation**: ±2.5% pitch + 0.94–1.0 gain jitter per press.
- **Pitch slider**: `speed = 2^{pitch×0.5}`, pitch ∈ [−1,1] (±half octave),
  clamp 0.25–4.
- **Normalization**: per-sample correction
  `clamp(pow(targetRMS/(rms×profileGain), 0.75), 0.5, min(1.8, 0.65/(peak×profileGain)))`;
  target = median press RMS across non-silent profiles clamped 0.04–0.12
  (fallback 0.06); press/release pair shares one correction bounded by both
  peaks; `normalizationReference=false` excludes a pack from the median.
- Press/release pairing: release variant chosen at press time and stored
  per-key so profile changes mid-hold can't cross wires. Muted press →
  remembered nil → silent release.
- Device switching: cpal device enumeration → IPC picker; stream rebuild on
  device loss with debounce.

## 4. Sound profiles & assets

- **Manifest**: Clicky's `profiles.json` schema:
  `{id, name, brand?, subtitle, color, samples[], releaseSamples?,
  keySamples{"7:44":{samples[], releaseSamples?}}, gain, provenance?,
  normalizationReference?}`. Enter 7:40 / keypad 7:88 alias same samples.
  WAV spec: mono 48 kHz PCM16, relative paths.
- **Bundled**:
  - 10 tplai MIT packs: alps-skcm-blue, drop-holy-panda, durock-alpaca,
    gateron-ink-black/-red/-turquoise-tealios, kailh-box-navy,
    novelkeys-cream, topre-unknown, ibm-buckling-spring. 5 generic presses +
    1–2 releases + space/enter/backspace pairs (121 WAVs). Ship
    `kbsim-MIT.txt` notice. Brands → manufacturer grouping.
  - 10 CC0 procedural banks matching Clicky's names/vibes (thocky, marbly,
    silent, poppy, clicky, bubble-wrap, clacky, creamy, deep-thock, office) —
    fresh synthesis, NOT Clicky's WAVs (permission-only). Press-only, 6
    variants via param jitter. Generator: `tools/gen_sounds.py` (stdlib,
    thock-proven chain: LP-noise transient + decaying sine body + optional
    ring; per-pack params; deterministic seed → reproducible). WAVs committed;
    build needs no Python.
  - Original-10 Clicky WAVs are NOT shippable (permission bound to Clicky).
  - Mouse pack (razer-orochi-v2, CC0) optionally bundled.
- **User imports**: pack dir with `pack.json` (thock-compatible superset:
  `name, license, version, default{press,release}, keys.<name>{press,release}`)
  → copied to `~/.local/share/clicky/packs/`; WAV/MP3/FLAC/OGG via symphonia;
  ≤15 s; missing/invalid → pack flagged, fallback to default.

## 5. Config

- Clicky `AppConfiguration` v1 at `~/.config/clicky/config.json`, atomic
  pretty JSON, debounced writes (250 ms), `schemaVersion==1` gate (unknown →
  error + backup, never overwrite), all numerics clamped on load, missing
  fields default (forward-compatible).
- Top-level: `schemaVersion, enabled, sound{profileID, volume, tone, pitch,
  spatial, spatialWidth, normalization, variation, homeRowSoftness,
  modifierSoundMode, outputDeviceUID, mouseSound, mouseVolume, enterSound,
  enterVolume, custom*}, visualizer{enabled, style, placement, theme, scale,
  offset, dismissDelay, comboTimeout, keepCombo, keepVisible}, general
  {launchAtLogin, shortcut{usage,modifiers,tapCount,interval}},
  keyOverrides{"page:usage":{profileID?,tone?,pitch?,volume?}},
  favorites[] (max 6 {id,name,sound,keyOverrides})`.
- Clamp ranges ported: unit 0–1, tone/pitch −1–1, scale 0.5–2, offset 0–150,
  dismissDelay 0.3–5, comboTimeout 0.3–30, tapCount 1–5, interval 0.3–3.
- Default toggle shortcut: Super + K ×3 taps in 1 s (ShortcutRecognizer:
  N taps same usage, exact modifier mask, inside window).

## 6. Tuning & soft modifiers

- Resolution order: key override → modifier policy → profile default.
- Trigger params resolved at event time: `{sample, gain = master ×
  press|release × override, pan, pitch × jitter, tone}`.
- **Soft modifiers**: `soft(0.25) | silent(0) | full(1)` + custom % and pitch;
  applies L and R independently (usages 224–231); modifier-only effect — the
  chorded key keeps normal timing/volume, no chord-detection delay. Six
  favorite presets (Clicky's defaults ported, user-editable).
- Per-key overrides: click key on 2D keyboard → volume/pitch sliders → live
  audition.
- Stereo pan from key geometry: `pan = ((x+w/2)/15)×2−1` over the 15-wide
  layout × spatialWidth (0.55 default headphone width).
- Gain/tone changes apply to new triggers only; master volume = mixer ramp.
  No pops.

## 7. UI (Tauri 2 + React/TS)

Dark, minimal, DMS-adjacent; standalone (no DMS dependency).

- **Home**: profile, enable toggle, master volume, preview.
- **Library**: manufacturer-grouped cards, search, preview, import.
- **Tuning**: tone/pitch/volume/spatial + per-key overrides (2D keyboard).
- **Modifiers**: mode, custom gain/pitch, sound picker, six presets.
- **Visualizers**: per-visualizer toggles + appearance.
- **Audio**: output-device picker, refresh, preview.
- IPC commands mirror thock's proven shape (get/set config, list packs,
  play_test, preview) + broadcast `config` event to UI + tray.

## 8. Visualizers (each optional, `--overlay` subprocess)

gtk4-layer-shell, layer `overlay`, click-through (empty input region),
anchored per niri layer rules:

- **keyboard**: 15-wide 6-row layout, press/release highlight, smooth anim.
- **keystroke**: pill stack of key NAME + modifier symbols only — never text.
- **combo**: modifier symbols + counter of non-modifier presses while held;
  resets on comboTimeout unless keepCombo.
- **bezel**: pulsing edge border.
- **3D keyboard** (milestone 5): transparent Tauri window + three.js,
  draggable/rotatable, animated presses. niri has no always-on-top → floating
  window or layer-shell surface; mechanism decided after milestone-5 probe.
  Fallback: in-app 3D view.

## 9. Desktop integration

- `.desktop` file; autostart opt-in (toggle writes
  `~/.config/autostart/clicky.desktop` → `clicky --daemon`).
- Tray via SNI (verified: watcher+host registered by waybar AND quickshell).
- `niri msg event-stream` optional hooks (e.g., pause on lock) — stretch.
- DMS coexists via tray; no plugin in v1.
- Fedora: RPM via cargo-packaging or tarball+install script (fish instructions).
- No GNOME Shell extensions, no X11, no root requirement.

## 10. Privacy

Offline always. Never: text reconstruction, keystroke storage/transmission,
telemetry, account, screenshot, mic. Only `EV_KEY` code + phase + device id
processed; FIFO to overlays carries key identity only. `docs/WAYLAND_INPUT.md`
documents exactly what's read and why.

## 11. Testing & performance

- **Automated** (cargo test): event normalization (synthetic `input_event`
  streams: dedup, repeat, L/R mods, SYN_DROPPED), evdev→HID table spot-checks,
  manifest/pack validation, config v1 round-trip + clamp + migration gate,
  trigger-resolution order + gain math (0.25, 0.7), queue overflow,
  mixer offline-render (voices, limiter ceiling, pan/tone).
- **Manual**: `docs/TESTING.md` Fedora44+niri matrix (capture in
  terminal/browser/Electron, dual keyboards, perm-denied degradation, overlap
  spam, modifiers, device hot-swap, suspend/resume, autostart, tray, DMS).
- **Perf**: `--diagnostics` reports buffer size + key→callback latency.
  Targets (measured, recorded in docs): <10 ms key→audio, <1% CPU idle,
  <50 MB RSS headless.

## 12. Docs to produce (milestone 1)

`docs/PRODUCT_SPEC.md` (Linux-adapted scope), `ARCHITECTURE.md`,
`WAYLAND_INPUT.md`, `AUDIO_ENGINE.md`, `IMPLEMENTATION_PLAN.md` — kept in
sync with implementation, not written ahead of truth.

## 13. Milestones

1. **Foundation**: rustup, scaffold, **feasibility probes**: evdev reads keys
   on niri (uaccess rule), cpal plays WAV with measured latency — gate before
   UI work. Write the 5 docs.
2. **Sound engine**: evdev → mixer → one MIT pack, press/release, master vol,
   enable/disable, config persist, tray. *Type anywhere, hear sounds.*
3. **Library**: 20 profiles, grouping/search/preview/import, per-key overrides.
4. **Tuning**: tone/pitch/pan, per-key tuning, soft modifiers, six presets.
5. **Visualizers**: keyboard, keystroke, combo, bezel (layer-shell), 3D.
6. **Integration**: device picker, hotplug, autostart, packaging, niri hooks,
   docs sync.

## 14. Out of scope v1

Head tracking (no Linux API — documented limitation, extensible interface),
mouse sounds (schema supports, ships later), DMS plugin, Windows/macOS,
media-key 2D layout entries (sounds only).

## Known risks

- uaccess rule needs root once — degraded mode covers failure.
- gtk4-layer-shell on niri 26.04 unverified until milestone-1/5 probe
  (niri documents wlr-layer-shell support).
- WebKitGTK under niri — verified working on Fedora+niri generally; confirmed
  in milestone-1 scaffold.
- Procedural bank quality is subjective — tuning params iterated in M3.
