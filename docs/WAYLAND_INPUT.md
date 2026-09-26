# Clicky Linux — Wayland Input

Condensed from design spec §2 + §10. What we read, why, and what we never do.

## Why evdev directly

Wayland has no global key-capture API for passive listeners:

- `InputCapture` portal — KVM-shaped (take/return control), wrong model.
- `RemoteDesktop` portal — injection-shaped, needs a session dialog + pipewire stream, wrong model.
- `global-shortcuts` — per-hotkey registration, not passive key events.

X11-style grabs (`rdev`) are a dead end on Wayland (verified via thock).
`libinput debug-events`-adjacent reading of `/dev/input/event*` works under
every compositor, including niri — the kernel delivers events to the
compositor *and* to any process with read access on the node.

## Permission model: udev `uaccess`

`udev/99-clicky-uaccess.rules`:

```
SUBSYSTEM=="input", KERNEL=="event*", ENV{ID_INPUT_KEYBOARD}=="1", TAG+="uaccess"
```

- logind grants the **active seat user** a POSIX ACL on keyboard event
  nodes only — no `input` group, no root daemon, no setuid.
- ACL applies only while the session is active; revocable by removing the
  rule + `udevadm trigger`.
- Install is an explicit guided opt-in: `tools/setup.sh --install` (or the
  UI shows the one-time `pkexec` command on `EACCES`).
- Degraded mode: permission denied → engine idles; library, preview, and
  customization remain fully usable.

## Threat model

| Consideration | Position |
|---|---|
| What we read | `EV_KEY` code + value (0/1/2) + device id. That's it. |
| Text leakage | Impossible by construction: no text path exists anywhere — no layout lookup, no keymap load, no string assembly. FIFO to overlays carries key identity (HID usage + phase) only. |
| Injection | Never — devices opened read-only, never `EVIOCGRAB`, no uinput. Kernel still delivers events to niri with zero interference. |
| Scope creep from uaccess | Documented tradeoff: while the rule is installed, *any* same-user process can read keyboard event nodes — true of all passive capture (evdev sniffing is trivially available to every keylogger anyway). The rule only exposes data already readable by the user's own session software. |
| Hotplug | udev monitor + inotify on `/dev/input`; re-enumerate on change, per-device held-state (`device_id`) prevents stuck modifiers. |
| Repeat/SYN_DROPPED | `value==2` suppressed (configurable); `SYN_DROPPED` → clear held-set and resync. |

## Key identity

evdev `KEY_*` → HID `"page:usage"` via static table (KEY_A=30 → `"7:4"`,
Enter=28 → `"7:40"`, modifiers 224–231 L/R distinct). Adopts Clicky's
identity verbatim → config v1 and per-key overrides are format-compatible.
Page 9 (mouse 1–3) and page 12 (consumer/media) supported as sound
triggers; media keys get sounds but aren't in the 2D layout.

## Milestone-1 probe record

Fill in from Task 1 (evdev probe) results: device list, ACL check output,
event stream sample, confirm compositor unaffected.
