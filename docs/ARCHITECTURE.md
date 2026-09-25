# Clicky Linux — Architecture

Condensed from the design spec §1–3. One daemon binary `clicky` plus a
`clicky-overlay` subprocess per visualizer. Engine threads outlive the
webview; the daemon is headless when the settings window is closed.

## Module map (`crates/clicky-core`)

| Module | Responsibility |
|---|---|
| `keymap` | evdev `KEY_*` → HID `"page:usage"` static table + `keyid()` |
| `input` | keyboard enumeration (EV_KEY + KEY_A..Z filter), epoll read loop, udev+inotify hotplug |
| `normalizer` | raw events → `KeyEvent`: dedup, repeat suppression, held-set, modifier bitmask, SYN_DROPPED resync |
| `engine` | `KeyEvent` → `Trigger` resolution: overrides → modifier policy → profile; release tracking; pan from key geometry |
| `mixer` | RT core: 96 voices, SPSC trigger queue (cap 1024), resample/tone/pan/ramps/limiter — no alloc/lock in render |
| `audio` | cpal stream ownership, device enum/switch, master ramp |
| `profiles` | `profiles.json` manifest (Clicky schema verbatim), pack loader/validator |
| `config` | `AppConfiguration` v1 serde, atomic store, clamps, debounce |
| `visual` | FIFO writer → `$XDG_RUNTIME_DIR/clicky/events` (only when a visualizer is enabled) |
| `diagnostics` | `--diagnostics`: devices, perm check, measured latency |

`crates/clicky` = thin binary: CLI dispatch + (later) Tauri tray/IPC/UI.
`crates/clicky-overlay` = gtk4-layer-shell visualizer client.

## Threads

```
udev monitor ─┐
              ▼
/dev/input/event* ──epoll──► [input thread]                [main thread]      [cpal render callback]
  (read-only, no grab)       normalizer ──► engine            Tauri UI          mixer.render(out)
                              │  KeyEvent      │  Trigger        │                ▲
                              │                ├───────────────► rtrb SPSC ───────┘
                              │                │                 (cap 1024,       zero alloc/lock
                              │                │                  drop-oldest)    registered samples
                              │                ▼                                 preloaded (max 2048)
                              │           visual::emit ──► $XDG_RUNTIME_DIR/clicky/events (FIFO)
                              │                                                  │
                              │                                                  ▼
                              │                                    clicky-overlay <kind> subprocess
                              │                                    (layer-shell, click-through;
                              │                                     supervisor restarts once)
                              ▼
                        config save (atomic, 250 ms debounce)
```

## Key contracts

- **Key identity**: HID `"page:usage"` string ("7:44" = Space); modifiers =
  page-7 usages 224–231 L/R distinct; mouse buttons page 9 usages 1–3.
  Same format as Clicky → config/overrides cross-compatible.
- **Trigger → mixer**: `Trigger{sample_idx, gain, pan, pitch, tone}` arrives
  fully resolved; render callback does no lookup, no alloc, no lock.
- **Gain**: `sliderVolume × manifestGain × normalization × variation × 2`;
  recorded releases ×0.7; soft modifiers ×0.25.
- **Profile switch** = atomic pointer swap; release variant chosen at press
  time and stored per-key so mid-hold switches can't cross wires.
- **Never**: `EVIOCGRAB`, input injection, key-text capture/storage.

## Process model

Launch = daemon: config → input thread → engine → mixer → cpal stream →
Tauri (tray via SNI + hidden settings window). Window close → hide; tray
Quit → exit. `--daemon` skips the UI entirely for autostart.
