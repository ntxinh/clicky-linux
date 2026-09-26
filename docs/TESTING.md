# Clicky Linux — Manual Test Matrix

Fedora Workstation 44 · Wayland · niri 26.04 · DankMaterialShell · fish.

Run against a release build (`cargo build --release --workspace`) or the
installed package (`sh tools/install.sh`). Checkbox rows are the pass/fail
gate for each release.

## 0. Prerequisites

- [ ] udev uaccess rule installed (one-time, needs sudo — see INSTALL.md):
      `sudo cp udev/99-clicky-uaccess.rules /etc/udev/rules.d/`,
      `sudo udevadm control --reload && sudo udevadm trigger`.
      The `trigger` step re-tags already-plugged keyboards — required; no
      replug/relogin needed.
- [ ] `getfacl /dev/input/event*` shows your user has `rw` on the keyboard
      nodes. `clicky --diagnostics` lists readable keyboards, 0 issues.
- [ ] `~/.local/bin` on PATH (`fish_add_path ~/.local/bin`).

## 1. Capture correctness

- [ ] Start `clicky --daemon`. Type in a **terminal** (ghostty/alacritty) →
      press + release sounds, correct keys.
- [ ] Type in **Firefox** address bar and a textarea → sounds.
- [ ] Type in an **Electron** app (VS Code) → sounds.
- [ ] Sounds follow the *physical* device: typing in a text field does not
      depend on which app is focused (system-wide capture).
- [ ] No interference: compositor keybinds (niri binds, DMS shell) still
      fire; typing latency in apps is unchanged (read-only evdev, no grab).

## 2. Dual keyboards

- [ ] Plug a second keyboard. `clicky status` → capture picks it up without
      restart (udev hotplug); typing on either device produces sound.
- [ ] Unplug the second keyboard while running → no crash, first keyboard
      still sounds; replug → works again.

## 3. Engine control (CLI + socket)

- [ ] `clicky disable` → typing silent; `clicky status` shows disabled.
- [ ] `clicky enable` → sounds back.
- [ ] `clicky profile <id>` switches banks live (type before/after).
- [ ] `clicky quit` → daemon exits cleanly; `pgrep clicky` empty;
      `ls $XDG_RUNTIME_DIR/clicky/control.sock` → gone (socket reaped);
      no `clicky-overlay` children left (`pgrep -af clicky-overlay`).
- [ ] Second `clicky` while one runs → forwards `show` to the live instance,
      exits (single-instance via socket, no double capture).

## 4. Config persistence

- [ ] Toggle profile/volume/modifier mode in the UI (or CLI) → quit →
      relaunch → settings restored from `~/.config/clicky/config.json`
      (atomic write — kill -9 mid-write must not corrupt).

## 5. Permission-denied degradation

- [ ] Move the udev rule away (`sudo rm /etc/udev/rules.d/99-clicky-uaccess.rules`,
      reload+trigger, replug) → `clicky --daemon` still starts, logs capture
      issues, `status` reports them; UI still opens: library, preview and
      tuning all work (preview is mixer-side, needs no evdev).
- [ ] Reinstall the rule *while the daemon runs* → capture recovers via the
      retry poll without restart.

## 6. Latency & load

- [ ] `clicky --diagnostics` reports negotiated buffer + device.
      Target: **<10 ms key→audio**; measured on this box **≈3.3 ms**
      trigger→DAC (see `docs/AUDIO_ENGINE.md` — 128-frame request honored,
      ~2.9 ms playback delay + 0.4 ms queue).
- [ ] Idle: `clicky --daemon` <1% CPU, <50 MB RSS (`status`/ps). No FIFO
      writer, no overlay children when all visualizers off.
- [ ] **Overlap spam**: mash keys / piano-roll 20+ keys → 96 voices steal
      oldest cleanly, no clipping warble (limiter), no crackle; `status`
      counters show drops only under absurd spam.

## 7. Modifiers & tuning

- [ ] Shift/Ctrl/Alt with modifier mode on → softer/distinct sound per
      configured gain/pitch.
- [ ] Per-key override (UI: select key → custom profile/tone/volume) →
      applies to that key only; clearing reverts.
- [ ] Modifier preset slots 0–5: save → change settings → recall →
      snapshot restored.

## 8. Output device

- [ ] UI device picker (or `set_output_device` IPC): switch to a different
      output → sounds move; switch back → restored. Config persists the
      chosen device.
- [ ] **Hot-swap**: unplug the active output (USB/Bluetooth) → stream
      rebuilds on the new default after debounce; in-flight sounds cut is
      acceptable (documented). *Note: single-output test boxes can only
      verify set_device; unplug-follow verified where hardware allows.*

## 9. Autostart

- [ ] UI toggle "Launch at login" ON → `~/.config/autostart/clicky.desktop`
      written (`Exec=clicky --daemon`, `X-GNOME-Autostart-enabled=true`).
- [ ] Log out/in → engine running headless (`clicky status` replies),
      settings window NOT open.
- [ ] Toggle OFF → file removed; next login no engine.

## 10. Suspend / resume

- [ ] `systemctl suspend` → resume → sounds still work; `status` replies,
      device possibly re-negotiated (stream rebuild is automatic).

## 11. Visualizers

Each kind is click-through except `keyboard3d`. Verify per kind via the UI
toggles (or `visualizer_set`):

- [ ] `keyboard`: 2D layout appears, highlights pressed/released keys,
      smooth animation; **click-through** (click into a window *through*
      the overlay → underlying app receives it).
- [ ] `keystrokes`: typed keys stream into view; click-through.
- [ ] `combo`: combo counter increments on streaks, resets per configured
      timeout; click-through.
- [ ] `bezel`: bezel animation on press; click-through.
- [ ] `keyboard3d`: 3D keyboard renders (cairo DrawingArea); **drag rotates**
      it — overlay-layer surfaces do receive pointer drags on niri 26.04
      (verified); keys animate on press.
- [ ] Toggle a kind off → its `clicky-overlay` child is reaped by the
      supervisor; kill a child manually → one respawn, then marked `failed`
      in `status`.
- [ ] `clicky quit` with all kinds on → every overlay exits.

## 12. Coexistence

- [ ] Run alongside DMS (DankMaterialShell) + waybar/quickshell: no input
      conflicts, no duplicate sounds, UI opens over the shell fine.
- [ ] Keybind integration: `niri`-spawned `clicky enable`/`disable`/`profile`
      works (CLI verbs against the socket — usable from DMS binds).

## 13. Packaging (installed-mode checks)

- [ ] `sh tools/install.sh --dry-run` prints correct dest paths.
- [ ] `sh tools/install.sh` → `which clicky` resolves, `clicky` launches,
      `~/.local/share/clicky/sounds` has all 20 profiles, desktop entry
      shows in app launcher with the amber icon.
- [ ] `sh tools/install.sh --uninstall` removes bins/sounds/desktop/autostart.
