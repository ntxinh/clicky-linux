# Clicky Linux Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A native offline mechanical-keyboard sound engine for Fedora 44 + niri + Wayland — press/release sounds in every app, driven by direct evdev input and a lock-free cpal mixer, with a Tauri 2 settings app.

**Architecture:** One daemon binary `clicky` (Tauri main thread + evdev input thread + cpal render callback, SPSC queues between them) + separate `clicky-overlay` binary (gtk4-layer-shell visualizers, spawned on demand). Sound profiles adopt Clicky's `profiles.json` manifest and HID `page:usage` key identity; config adopts Clicky's `AppConfiguration` v1.

**Tech Stack:** Rust (stable via rustup), Tauri 2, React+TS+Vite, cpal, evdev, rtrb, serde_json, symphonia (imports), gtk4 + gtk4-layer-shell (overlay), Python stdlib (sound generator).

**Spec:** `docs/superpowers/specs/2026-09-26-clicky-linux-design.md`

## Global Constraints

- Key identity is the HID `"page:usage"` string (e.g. `"7:44"` for Space); modifiers are page-7 usages 224–231 (L/R distinct); mouse buttons are page 9 usages 1–3.
- WAV contract: mono 48 kHz PCM16; max 2048 registered samples; imports ≤ 15 s.
- Mixer: 96 voices, trigger queue cap 1024, linear resample, constant-power pan, one-pole tone, 0.5 ms attack / 2 ms release ramps, soft limiter knee −3.1 dBFS, oldest-voice steal. Render callback: no alloc, no lock.
- Gain math: `gain = sliderVolume × manifestGain × normalization × variation × 2`; recorded releases ×0.7; soft modifiers ×0.25; pitch `2^(pitch×0.5)` clamped 0.25–4; variation = ±2.5% pitch + 0.94–1.0 gain jitter.
- NEVER `EVIOCGRAB`, never inject input, never store/transmit key text. Only `EV_KEY` code+phase+device_id processed.
- Config: `~/.config/clicky/config.json`, `schemaVersion: 1`, atomic pretty JSON, debounced (250 ms), all numerics clamped on load, missing fields default.
- Bundled audio: 10 tplai MIT packs (ship `Assets/Licenses/kbsim-MIT.txt`) + 10 fresh CC0 synthesized banks. Clicky's original 10 WAV banks are NOT redistributable — do not copy them.
- No placeholders: every step names real code/tests.

## File Structure

```
Cargo.toml                  workspace: [clicky, clicky-overlay]
crates/clicky/
  src/main.rs               CLI dispatch (--daemon/--overlay/--diagnostics/enable/disable/profile)
  src/keymap.rs             evdev KEY_* → (page,usage) table + keyid()
  src/input.rs              device enumeration, epoll read loop, hotplug
  src/normalizer.rs         KeyEvent → NormalEvent (repeat/held/modifier logic)
  src/mixer.rs              lock-free mixer core (offline-renderable)
  src/audio.rs              cpal stream, device enum/rebuild, mixer wiring
  src/profiles.rs           profiles.json manifest, pack loader, validation
  src/engine.rs             resolution: NormalEvent → Trigger; FIFO fan-out
  src/config.rs             AppConfiguration v1 serde + store + clamps
  src/visual.rs             FIFO writer to $XDG_RUNTIME_DIR/clicky/events
  src/tray.rs               SNI tray icon + menu
  src/ipc.rs                #[tauri::command] surface + AppState
  src/diagnostics.rs        --diagnostics report
crates/clicky-overlay/
  src/main.rs               gtk4-layer-shell surfaces: keyboard|keystrokes|combo|bezel
ui/                         React+TS+Vite settings app
sounds/<profile>/           WAVs; profiles.json at sounds/
sounds/LICENSES/kbsim-MIT.txt
tools/gen_sounds.py         CC0 bank synthesizer
udev/99-clicky-uaccess.rules
docs/{PRODUCT_SPEC,ARCHITECTURE,WAYLAND_INPUT,AUDIO_ENGINE,IMPLEMENTATION_PLAN,TESTING}.md
```

---

### Task 0: Toolchain, workspace scaffold, docs skeleton

**Files:**
- Create: `Cargo.toml`, `crates/clicky/Cargo.toml`, `crates/clicky/src/main.rs`, `crates/clicky-overlay/Cargo.toml`, `crates/clicky-overlay/src/main.rs`, `.gitignore`, five `docs/*.md` skeletons.

- [ ] **Step 1: Install toolchain** `curl --proto '=https' -sSf https://sh.rustup.rs | sh -s -- -y` then `rustup component add clippy rustfmt`. Install system deps for Tauri+gtk4: `sudo dnf install webkit2gtk4.1-devel openssl-devel gtk4-devel gtk4-layer-shell-devel libappindicator-gtk3-devel librsvg2-devel alsa-lib-devel libudev-devel nodejs` (Fedora 44 package names; `pnpm` via `npm i -g pnpm`).
- [ ] **Step 2: Scaffold workspace.** Root `Cargo.toml`:
```toml
[workspace]
members = ["crates/clicky", "crates/clicky-overlay"]
resolver = "2"

[workspace.package]
edition = "2021"
license = "MIT"
```
`crates/clicky/Cargo.toml` deps: `evdev`, `rtrb`, `cpal`, `serde`, `serde_json`, `symphonia`, `tauri` (+features `tray-icon`), `tauri-build`, `anyhow`, `thiserror`, `fastrand`, `udev`, `nix` (epoll), `directories`. `crates/clicky-overlay` deps: `gtk4`, `gtk4-layer-shell`.
- [ ] **Step 3: `cargo build` green; commit.** Minimal `fn main()` in both bins.
- [ ] **Step 4: Write docs skeletons** — `PRODUCT_SPEC.md` (adapted scope from spec §1), `ARCHITECTURE.md` (module map + thread diagram), `WAYLAND_INPUT.md` (evdev choice, uaccess model, threat model, no-portal explanation), `AUDIO_ENGINE.md` (mixer spec + latency table to fill), `IMPLEMENTATION_PLAN.md` (milestone list pointing at this plan). Content = condensed spec sections; each ~1 page.
- [ ] **Step 5: Commit** `git add -A && git commit -m "chore: workspace scaffold + docs skeleton"`.

---

### Task 1: SPIKE — evdev readability under niri (feasibility gate 1)

**Files:**
- Create: `crates/clicky/src/bin/evdev_probe.rs` (throwaway, kept as diagnostics basis).

- [ ] **Step 1: Write probe** — enumerate `/dev/input/event*`, print name + `supported_keys().contains(Key::KEY_A)` filter, try `Device::open`:
```rust
for entry in std::fs::read_dir("/dev/input")?.flatten() {
    let p = entry.path();
    if !p.file_name().unwrap().to_str().unwrap().starts_with("event") { continue }
    match evdev::Device::open(&p) {
        Ok(d) if d.supported_keys().map_or(false, |k| k.contains(evdev::Key::KEY_A)) =>
            println!("KBD {:?} {}", p, d.name().unwrap_or("?")),
        Ok(_) => println!("non-kbd {:?}", p),
        Err(e) => println!("denied {:?}: {}", p, e),
    }
}
```
- [ ] **Step 2: Run before uaccess rule** → expect `denied` on all (verified: user lacks access). Record output.
- [ ] **Step 3: Install udev rule** (as documented opt-in): `udev/99-clicky-uaccess.rules` = `SUBSYSTEM=="input", KERNEL=="event*", ENV{ID_INPUT_KEYBOARD}=="1", TAG+="uaccess"` → `sudo cp` + `sudo udevadm control --reload && sudo udevadm trigger`. Re-login or `udevadm trigger` may need replug; verify with `getfacl /dev/input/eventN | grep exodia`.
- [ ] **Step 4: Re-run probe** → keyboards readable. Add a 10 s event-print loop on the first kbd device; type in another window; confirm press/release/repeat(value=2) events arrive and normal typing still works (no grab).
- [ ] **Step 5: Record findings in `docs/WAYLAND_INPUT.md`** (devices, ACL mechanics, latency-irrelevant result). Commit `feat(spike): evdev probe proves niri-global capture via uaccess`.

---

### Task 2: SPIKE — cpal playback latency (feasibility gate 2)

**Files:**
- Create: `crates/clicky/src/bin/audio_probe.rs` (throwaway).
- Test asset: any 48 kHz mono WAV (generate 30 ms click via `tools/gen_sounds.py` fragment or `sox -n`).

- [ ] **Step 1: Probe** — `cpal::default_host()`, `default_output_device()`, `default_output_config()` → print device name, sample rate, buffer-size range. Build stream with `BufferSize::Fixed(128)` fallback default; play WAV via a trivial one-shot voice (ring-buffer the decoded f32 into callback). Measure: write callback timestamps to show frames/callback; log `StreamInstant` delta between trigger push and first nonzero output sample.
- [ ] **Step 2: Run; record** effective buffer size + host backend used (ALSA→pipewire-alsa vs pulse host — try `cpal::host_from_id` variants if default is poor).
- [ ] **Step 3: Record numbers in `docs/AUDIO_ENGINE.md`.** If >15 ms, try smaller buffer / pulse host / `BufferSize::Fixed(64)`; document best config.
- [ ] **Step 4: Commit** `feat(spike): cpal stream + measured latency on PipeWire`.

---

### Task 3: `keymap.rs` — evdev→HID table

**Interfaces:** Produces `pub fn keyid(code: u16) -> Option<&'static str>` returning `"page:usage"`, and `pub fn hid_usage(code: u16) -> Option<(u8,u16)>`.

- [ ] **Step 1: Failing test** (`keymap.rs` `#[cfg(test)]`):
```rust
#[test] fn letters_digits_mods() {
    assert_eq!(keyid(30), Some("7:4"));   // KEY_A
    assert_eq!(keyid(11), Some("7:39"));  // KEY_0
    assert_eq!(keyid(28), Some("7:40"));  // KEY_ENTER
    assert_eq!(keyid(57), Some("7:44"));  // KEY_SPACE
    assert_eq!(keyid(42), Some("7:225")); // KEY_LEFTSHIFT
    assert_eq!(keyid(54), Some("7:229")); // KEY_RIGHTSHIFT
    assert_eq!(keyid(125), Some("7:227"));// KEY_LEFTMETA
    assert_eq!(keyid(96), Some("7:88"));  // KEY_KPENTER
    assert_eq!(keyid(115), Some("12:233"));// KEY_VOLUMEUP
    assert_eq!(keyid(272), Some("9:1"));  // BTN_LEFT
    assert_eq!(keyid(9999), None);
}
```
- [ ] **Step 2: Verify fail** `cargo test keymap` → compile error (fn missing).
- [ ] **Step 3: Implement** — static `&[(u16,u8,u16)]` table covering: alpha row, digits, punctuation, Enter/Tab/Space/Backspace/Esc/CapsLock, F1–F24 (59–68→7:58–67, 87,88→7:68,69, 183–194→7:104–115), nav cluster (102→74,103→82,104→75,105→80,106→79,107→77,108→81,109→78,110→73,111→76), numpad (55→85,71–73→89–91,74→86,75–77→92–94,78→87,79–81→95–97,82→98,83→99,96→88,98→84,117→103,121→133), modifiers (29→224,42→225,56→226,125→227,97→228,54→229,100→230,126→231), 99→70,69→83,70→71,119→72,116→102,127→101; page 12: 113→226,114→234,115→233,164→205,163→181,165→182,225→111,224→112; page 9: 272→1,273→2,274→3. `keyid` formats `"p:u"`.
- [ ] **Step 4: Pass; commit** `feat: evdev→HID usage keymap`.


### Task 4: `normalizer.rs` — event normalization

**Interfaces:** Consumes raw `(code: u16, value: i32, device: u32)`. Produces `pub enum Phase{Press,Release}`; `pub struct KeyEvent{pub keyid: &'static str, pub usage: u16, pub page: u8, pub phase: Phase, pub modifiers: u8}`; `pub struct Normalizer` with `fn feed(&mut self, device: u32, code: u16, value: i32, allow_repeat: bool) -> Option<KeyEvent>`.

- [ ] **Step 1: Failing tests:**
```rust
// press→emit; repeat(value=2)→None; release after press→emit; orphan release→None
// value=2 with allow_repeat=true → Some(Press)
// modifier press sets modifiers bitmask on subsequent events; L vs R distinct usages
// SYN_DROPPED helper: clear_held() makes previously-held key's next press emit (not swallowed)
```
- [ ] **Step 2: Fail. Step 3: Implement** — held-set `HashSet<(u32,u16)>`, modifiers bitmask = bit per modifier usage (224–231 → bits 0–7), `MODIFIER_USAGES` range check. value 1→insert+emit if new (dedup repeat-downs), 2→emit iff `allow_repeat`, 0→emit iff was held (orphan drop).
- [ ] **Step 4: Pass; commit** `feat: input normalizer (dedup/repeat/modifiers/orphans)`.

---

### Task 5: `input.rs` — device enumeration + epoll reader

**Interfaces:** Produces `pub fn keyboard_devices() -> Vec<Device>` (EV_KEY+KEY_A filter); `pub fn run(devices: Vec<Device>, cb: impl FnMut(u32,u16,i32), stop: Arc<AtomicBool>)` — epoll loop, device index as `device: u32`; `pub struct Hotplug` wrapping `udev::Monitor` + inotify on `/dev/input` → returns changed paths.

- [ ] **Step 1: Unit test** for `is_keyboard()` predicate on a mock-able signature `fn is_keyboard(d: &Device) -> bool` — test via real devices skipped in CI (`#[ignore]` or gate on env var); logic test = table of `(supported_keys sets)` can't be mocked easily → keep predicate trivial (`.supported_keys()` contains `KEY_A`..`KEY_Z` all) and cover enumeration by an ignored manual test.
- [ ] **Step 2: Implement** epoll loop over device fds (`AsRawFd`), `fetch_events()` drain per ready fd, `SYN_DROPPED` → `normalizer.clear_held(device)`. Hotplug: debounce 300 ms → re-enumerate, open new kbd devices, drop vanished.
- [ ] **Step 3: Manual verify** — run `clicky --diagnostics`-style print binary: type in another app → events stream; unplug/replug keyboard → device re-appears. Record in WAYLAND_INPUT.md.
- [ ] **Step 4: Commit** `feat: evdev device enumeration + epoll reader + hotplug`.

---

### Task 6: `mixer.rs` — the RT core (offline-renderable)

**Interfaces:** `pub const MAX_VOICES: usize = 96; pub const QUEUE_CAP: usize = 1024; pub const MAX_SAMPLES: usize = 2048;` `pub struct Trigger{pub sample:u16, pub gain:f32, pub pan:f32, pub pitch:f32, pub tone:f32}` `pub struct Mixer{…}` with `register(&mut self, pcm: Arc<[f32]>, src_rate: u32) -> u16` (stores sample + rate; per-voice step = `src_rate/out_rate × pitch`), `enqueue(&mut self, t: Trigger) -> bool` (SPSC via rtrb inside), `render(&mut self, out: &mut [f32], channels: usize)` (called from cpal callback AND tests), `stats() -> Stats{rendered,accepted,dropped,stolen}`.

- [ ] **Step 1: Failing tests:**
```rust
// silence in → silence out; trigger sine → nonzero out; voice steal at 97th
// pan ±1 → energy only on one channel (constant-power: center → both ±3dB)
// limiter: amplitude never exceeds 0.9999 even with 96 full-gain voices
// queue full → enqueue returns false, dropped counter increments
// tone=−1 darkens (spectral centroid lower than tone=0), tone=+1 brighter
```
- [ ] **Step 2: Fail. Step 3: Implement** — voices array `[Voice;96]`, `Voice{sample:u16,pos:f32,step:f32,gain:f32,pan:f32,tone_coeff/filt_state,ramp}`; render: per voice linear-interp fetch `pcm[pos]`, apply one-pole `y+=a(x−y)` with cutoff `18000×0.06^{-tone}` (tone>0 adds `0.8×(dry−filtered)`), constant-power `l=cos(θ) g, r=sin(θ) g, θ=(pan+1)π/4`, ramps, oldest steal (track `age`), soft limiter on summed stereo. `register` stores `Arc<[f32]>`; `enqueue` is producer side of embedded `rtrb`.
- [ ] **Step 4: Pass; commit** `feat: lock-free mixer core (96 voices, offline-renderable)`.

---

### Task 7: `profiles.rs` — manifest + sample loading

**Interfaces:** `pub struct Profile{id,name,brand,subtitle,color,gain,normalization_reference, presses:Vec<u16>, releases:Vec<u16>, keys:HashMap<String,KeySamples>}` where `u16` = mixer sample idx; `pub fn load_manifest(dir:&Path, mixer:&mut Mixer) -> Result<Vec<Profile>>`; `pub struct KeySamples{presses:Vec<u16>,releases:Vec<u16>}`; `fn validate(entry) -> Result<()>` (files exist, decode mono f32, ≤15 s).

- [ ] **Step 1: Failing tests** — fixture dir with tiny generated WAVs (write via `hound` dev-dep or embed bytes): manifest parses into Profile; missing WAV → `Err` naming file; >15 s → Err; `keySamples["7:44"]` lands in `keys`; `releaseSamples:null` → empty releases vec.
- [ ] **Step 2: Fail. Step 3: Implement** — serde types matching Clicky schema verbatim (`id,name,brand?,subtitle,color,samples,releaseSamples?,keySamples?,gain,provenance?,normalizationReference?`); WAV decode via symphonia (mono-convert, resample to engine rate once at load if ≠48k using mixer-resampled output — simplest: store at source rate, mixer linear-resamples anyway by design: store `src_rate` per sample. Decide: store `Arc<[f32]>` + rate in a `SampleBuf` registered with mixer).
- [ ] **Step 4: Pass; commit** `feat: profiles.json manifest + sample registry`.

---

### Task 8: `config.rs` — AppConfiguration v1

**Interfaces:** `pub struct AppConfiguration{schema_version,enabled,sound:SoundSettings,visualizer:VisualizerSettings,general:GeneralSettings,key_overrides:HashMap<String,KeyOverride>,favorites:Vec<Favorite>}` (serde camelCase field names per Clicky); `pub struct Store{path}` with `load()->Result<Self>`, `save_debounced(&mut self, cfg)` (250 ms), `validated()` clamps.

- [ ] **Step 1: Failing tests** — round-trip serialize→parse→equal; unknown `schemaVersion` → `Err` + `.bak` written; out-of-range values clamped (`volume:2.0→1.0`, `tone:−9→−1`, `dismissDelay:99→5`); missing fields → defaults; 7th favorite → truncated to 6.
- [ ] **Step 2: Fail. Step 3: Implement** serde structs (rename_all camelCase, `#[serde(default)]` everywhere), atomic save (tmp+rename), field set + clamp ranges copied from spec §5.
- [ ] **Step 4: Pass; commit** `feat: config v1 store (Clicky-compatible schema)`.

---

### Task 9: `engine.rs` — event→trigger resolution

**Interfaces:** Consumes `KeyEvent` (Task 4), `AppConfiguration` (8), `Vec<Profile>` (7). Produces `Trigger` (6) into `Mixer::enqueue` + optional `visual::emit`. `pub struct Engine` with `fn on_key(&mut self, ev: &KeyEvent)`, `fn set_profile(&mut self, idx)`, `fn update_config(&mut self, cfg)`, `fn preview(&mut self, keyid: &str)`.

- [ ] **Step 1: Failing tests** (mixer offline-render assertions):
```rust
// normal key → trigger queued with gain = vol×norm×var×2 bounds-check
// modifier usage 224 → gain×0.25 when modifierSoundMode="soft"
// release event on profile WITH releaseSamples → release trigger ×0.7
// release on press-only profile → nothing
// per-key override volume → overrides stack correctly
// variant selection: 200 presses → >1 distinct sample used, never same twice consecutively
// enabled=false → no triggers; disabled mid-hold → release suppressed (recorded nil)
```
- [ ] **Step 2: Fail. Step 3: Implement** — AudioSampleSelector (random ≠ previous), AudioReleaseTracker (`HashMap<(device,usage), Option<u16>>` chosen at press), normalization table computed at profile load (median targetRMS formula from spec §3), modifier policy, override merge order (override → modifier → profile default), pan from `KeyboardLayout` geometry table (x,width per usage over 15 cols → `pan=((x+w/2)/15)×2−1` × spatialWidth).
- [ ] **Step 4: Pass; commit** `feat: trigger resolution engine (profiles, modifiers, overrides)`.

---

### Task 10: `audio.rs` — cpal stream management

**Interfaces:** `pub struct Audio{mixer: Mixer (owns consumer side), stream: cpal::Stream}`; `pub fn start(cfg_out_device: Option<&str>) -> Result<Audio>`; `fn set_device(&mut self, name:&str)->Result<()>`; `fn devices() -> Vec<String>`; `fn set_master(&mut self, v:f32)` (3 ms slew inside mixer); `fn set_enabled(&mut self,bool)` (ramped).

- [ ] **Step 1: Implement** — move Mixer into `build_output_stream` data callback (`move` closure; consumer+voices live in callback; producer `rtrb::Producer` stays in Audio handle given to Engine). Device switch = drop+rebuild stream, re-register samples (registry lives outside callback; voices reset — acceptable: sounds in flight cut on switch, document).
- [ ] **Step 2: Test** — `#[ignore]`-gated: stream starts, `stats().rendered` grows after enqueuing trigger. Offline-render tests already cover mixer logic.
- [ ] **Step 3: Commit** `feat: cpal output stream + device switching`.

---

### Task 11: Milestone-2 wire-up — daemon mode + tray

**Interfaces:** `main.rs` dispatch: `--daemon`/default → `run_app()`; tray menu items `enable`, `pack:<id>` radio, `Settings…`, `Quit`. `Engine` drives `audio.producer`.

- [ ] **Step 1: Copy one MIT pack** — copy `Assets/Sounds/alps-skcm-blue` (or smallest, ~12 WAVs) + build `sounds/profiles.json` with just it + `LICENSES/kbsim-MIT.txt`. Loader reads `sounds/` dir next to binary (dev) / `~/.local/share/clicky/` (installed) — manifest path via `directories::ProjectDirs` + fallback `./sounds`.
- [ ] **Step 2: Daemon main loop**: load config → profiles → start audio → spawn input thread (Normalizer → Engine → mixer producer) → tray. Toggle enable from tray flips `config.enabled` (engine + mixer ramp).
- [ ] **Step 3: MANUAL GATE** — run `cargo run -- --daemon`, type in VS Code/terminal/Firefox: hear clicks; toggle off→silent; release sounds on MIT pack. Verify `--diagnostics` prints devices/perms/latency. **Do not proceed until this works.**
- [ ] **Step 4: Commit** `feat: daemon mode — system-wide typing sounds`.

---

### Task 12: `tools/gen_sounds.py` + 10 CC0 banks

**Interfaces:** `python3 tools/gen_sounds.py sounds` → per bank: WAVs + entry merged into `sounds/profiles.json` (schema = Task 7).

- [ ] **Step 1: Implement synth** (thock chain, stdlib `wave`/`math`/`random`): transient = noise × one-pole LP(cut) × exp-decay; body = sine(hz)×exp-decay w/ 15% pitch drop; optional ring sine; normalize, 2 ms fade, 48 kHz PCM16 mono. Deterministic seeds.
- [ ] **Step 2: 10 bank param dicts** targeting Clicky's names/vibes: thocky (185 Hz body, warm), marbly (220 Hz round), silent (210 Hz, low gains), poppy (450 Hz snap), clicky (340 Hz + 2.4 kHz ring), bubble-wrap (irregular transient burst), clacky (300 Hz sharp), creamy (200 Hz soft LP), deep-thock (95 Hz long), office (muted mid). 6 press variants each (jitter seed), no releases.
- [ ] **Step 3: Generate + audition** via `--diagnostics` preview command; sanity: each bank loads, plays.
- [ ] **Step 4: Copy remaining 9 tplai packs** from clicky `Assets/Sounds/{drop-holy-panda,durock-alpaca,gateron-ink-black,gateron-ink-red,gateron-turquoise-tealios,kailh-box-navy,novelkeys-cream,topre-unknown,ibm-buckling-spring}` + manifest entries (ids/brands verbatim from Clicky's profiles.json — copy the JSON entries, adjust paths). All 20 load in loader test.
- [ ] **Step 5: Commit** `feat: 20 bundled profiles (10 MIT recorded + 10 CC0 synthesized)`.

---

### Task 13: User pack import

**Interfaces:** `pub fn import_pack(dir:&Path)->Result<Profile>` — reads `pack.json` (thock superset: `name,license?,version?,default{press,release},keys{<name>:{press,release}}`), validates via Task 7 path, copies to `~/.local/share/clicky/packs/<id>/`, merges into manifest list; symphonia decodes wav/mp3/flac/ogg ≤15 s.

- [ ] **Step 1: Failing tests** — fixture pack.json + wavs → imports, appears in `list_packs`, plays preview; bad manifest → error; 20 s file → rejected; unknown key names tolerated (dropped with warning).
- [ ] **Step 2: Fail. Step 3: Implement. Step 4: Pass.**
- [ ] **Step 5: Commit** `feat: user sound-pack import (pack.json)`.

---

### Task 14: Tauri window + IPC + React UI

**Interfaces:** Commands: `get_config, set_enabled, set_volume, set_tone, set_pitch, set_spatial, set_profile, list_profiles, preview(keyid?,profile?), set_modifier_mode, set_modifier_gain, set_modifier_pitch, set_key_override(keyid,fields), clear_key_override, list_devices, set_output_device, set_launch_at_login, import_pack(path), get_diagnostics, visualizer_set(kind,enabled)`. Event: `config` broadcast after every mutation (thock's proven pattern).

- [ ] **Step 1: `tauri init` into `ui/`** (React+TS template via `pnpm create tauri-app` non-interactive flags or manual vite scaffold + `tauri.conf.json` pointing at `ui/dist`; `beforeDevCommand`/`beforeBuildCommand` pnpm).
- [ ] **Step 2: `ipc.rs`** — `AppState{engine,config_store,audio}` behind `Mutex` (fine: IPC thread ≠ audio thread); each command mutates engine/config → `save_debounced` → emit `config`.
- [ ] **Step 3: UI pages** — Home (profile name, enable switch, volume slider, Preview btn), Library (cards grouped by `brand`, search box, click→select+preview, Import button → file dialog), Tuning (tone/pitch/spatial sliders + 2D keyboard SVG: click key→override sliders→audition), Modifiers (mode select, gain/pitch, six preset slots), Visualizers (per-kind toggles), Audio (device dropdown + refresh + preview). Dark theme CSS matching DMS vibe (#111 sur-2, amber accent).
- [ ] **Step 4: Tray Settings item shows window; close→hide.**
- [ ] **Step 5: Manual verify** — sliders change live sound; profile switch instant; restart → persisted.
- [ ] **Step 6: Commit** `feat: Tauri settings UI + IPC`.

---

### Task 15: `visual.rs` + `clicky-overlay` (keyboard/keystroke/combo/bezel)

**Interfaces:** `visual.rs`: `pub fn emit(ev:&KeyEvent)` → writes `"{keyid} {phase}\n"` lines to `$XDG_RUNTIME_DIR/clicky/events` FIFO (created lazily when any visualizer enabled; nonblocking, reader-absent → drop). `clicky-overlay <kind>` binary: gtk4 app, `Layer::Overlay`, click-through input region, anchor per kind; reads FIFO, renders.

- [ ] **Step 1: FIFO writer + test** — write/read round-trip on tmpdir; reader absent → no block/panic.
- [ ] **Step 2: Overlay kinds**: `keyboard` (SVG/CSS 15-col layout, press→amber class, release→fade), `keystrokes` (pill stack, key NAME labels e.g. "Space", "Shift+K" never text), `combo` (modifier symbols + counter of non-mod presses while held, reset after `comboTimeout`), `bezel` (CSS pulsing border on monitor-sized surface).
- [ ] **Step 3: Daemon supervision**: `config.visualizer.<kind>.enabled` → spawn/reap `clicky-overlay <kind>`; crash → one restart → UI error state.
- [ ] **Step 4: Manual verify on niri** — surfaces appear overlay-layer, click-through confirmed (window under overlay still focusable), niri `layers` list shows them.
- [ ] **Step 5: Commit** `feat: layer-shell visualizer overlays`.

---

### Task 16: 3D keyboard visualizer

- [ ] **Step 1: Layer-shell probe** — gtk4-layer-shell surface with `KeyboardMode::Exclusive`/OnDemand + pointer input on niri: can it receive drags? If yes → overlay binary gains `keyboard3d` kind (gtk GL area or fallback: launch Tauri window variant). If no → 3D view lives inside settings window (three.js canvas, drag-rotate, animated presses fed by live events via IPC).
- [ ] **Step 2: Implement chosen path** — three.js box-geometry keyboard from `KeyboardLayout` coords, drag yaw/pitch, key-press animation from event stream.
- [ ] **Step 3: Manual verify drag + animation.**
- [ ] **Step 4: Commit** `feat: 3D keyboard`.

---

### Task 17: Integration & packaging

- [ ] **Step 1: `.desktop`** + install script (`install.sh` fish-compatible: binary→`~/.local/bin`, desktop file, udev rule guided install prompt, `sounds/`→`~/.local/share/clicky`).
- [ ] **Step 2: Autostart toggle** → `~/.config/autostart/clicky.desktop` (Exec=`clicky --daemon`).
- [ ] **Step 3: `niri msg event-stream` optional hook** — pause-on-lock (`overview-state`/inhibit) if trivially parseable; else document skip.
- [ ] **Step 4: Device hot-swap verify** — switch output device in UI; unplug default → engine follows.
- [ ] **Step 5: `docs/TESTING.md`** manual matrix filled from spec §13; run checklist.
- [ ] **Step 6: RPM** — `cargo install cargo-generate-rpm` or fpm; else tarball + install.sh documented.
- [ ] **Step 7: Commit** `feat: packaging, autostart, integration docs`.

---

## Self-review results

- Spec §2 input → Tasks 1,3,4,5. §3 audio → 2,6,7,10,14(device IPC). §4 profiles → 7,12,13. §5 config → 8,14. §6 modifiers/tuning → 9,14. §7 UI → 14. §8 visualizers → 15,16. §9 integration → 17. §10 privacy → architecture (no text path exists anywhere). §11 testing → per-task tests + Task 17 matrix. §13 milestones → Tasks ordered M1(0–5)→M2(6–11)→M3(7 done,12,13,14-partial)→M4(9 done,14)→M5(15,16)→M6(17).
- Known gap noted: `SampleBuf` rate carried into `register` (Task 7 decision: mixer `register` takes `(pcm, src_rate)`, step computed per-voice from `out_rate`).
