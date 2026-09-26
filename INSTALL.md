# Install — Clicky for Linux

Fedora 44 + Wayland + niri. System-wide mechanical-keyboard sounds, fully
offline.

## Option A — RPM (recommended)

```fish
sudo dnf install dist/clicky-0.1.0-1.x86_64.rpm
```

The RPM installs the udev rule to `/usr/lib/udev/rules.d/` and runs
`udevadm control --reload && udevadm trigger` in `%post` — so already-plugged
keyboards get access immediately, no extra step.

## Option B — `install.sh` (user-level, no package manager)

```fish
sh tools/install.sh          # binaries→~/.local/bin, sounds→~/.local/share/clicky
```

Then **one sudo step** grants keyboard access — the script prints it:

```fish
sudo cp udev/99-clicky-uaccess.rules /etc/udev/rules.d/99-clicky-uaccess.rules
sudo udevadm control --reload && sudo udevadm trigger
```

> **`udevadm trigger` is the step people skip — and the one that makes it
> work.** The rule only tags devices *when udev processes them*. Your
> keyboards were enumerated before the rule landed, so without a trigger they
> have no `uaccess` tag and logind never writes you an ACL — `capture: denied`,
> no sound. `trigger` re-processes them in place; **no replug, no relogin
> needed.**

## Verify (2 seconds)

```fish
getfacl /dev/input/event* | grep "$USER"     # → user:you:rw- on keyboard nodes
clicky --daemon &                            # start the daemon
clicky status                                # → capture:live, keyboards:N
# now type anywhere → sound
```

If `capture:denied` after install, run the `trigger` line above, then re-check.

## Launching

```fish
clicky --daemon      # headless engine (autostartable — UI toggle writes it)
clicky               # settings window (single-instance; CLI controls it too)
clicky status|enable|disable|profile <name>|quit
```

Autostart: UI → Settings → "Launch at login" writes
`~/.config/autostart/clicky.desktop` (`Exec=clicky --daemon`).

## If it's still silent

- `clicky status` → `capture` must be `live` (denied → run `udevadm trigger`).
- `enabled` must be `true` (`clicky enable`).
- `audio_device` should name your real output; check volume isn't 0.
- `keyboards: 0` with the rule installed → `sudo udevadm trigger`, then verify
  `getfacl` shows your user — if the ACL still doesn't appear, your logind
  session isn't `active` (check `loginctl`); fallback is
  `sudo usermod -aG input $USER` + relogin (broader, but permanent).

## Uninstall

```fish
sh tools/install.sh --uninstall        # user files
sudo dnf remove clicky                 # or the RPM
sudo rm -f /etc/udev/rules.d/99-clicky-uaccess.rules   # leftover rule (manual install)
```
