#!/bin/sh
# clicky user-level install — POSIX sh, safe under `fish -c` or `sh`.
# Everything lands in $HOME; the only root step is the udev rule, which is
# printed (or run with --udev) but never silently sudoed.
#
#   sh tools/install.sh            install user-level files
#   sh tools/install.sh --udev     also install the uaccess rule (sudo)
#   sh tools/install.sh --dry-run  print what would be copied
#   sh tools/install.sh --uninstall

set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD="$ROOT/target/release"
PREFIX="${PREFIX:-$HOME/.local}"
BINDIR="$PREFIX/bin"
SHARE="$PREFIX/share/clicky"
APPS="$PREFIX/share/applications"
ICONS="$PREFIX/share/icons/hicolor/scalable/apps"
RULE="$ROOT/udev/99-clicky-uaccess.rules"
RULE_DST="/etc/udev/rules.d/99-clicky-uaccess.rules"

DO_UDEV=0 DRY=0 UNINSTALL=0
for a in "$@"; do
    case "$a" in
        --udev) DO_UDEV=1 ;;
        --dry-run) DRY=1 ;;
        --uninstall) UNINSTALL=1 ;;
        *) echo "unknown flag: $a" >&2; exit 2 ;;
    esac
done

put() { # put <src> <dst>
    if [ "$DRY" = 1 ]; then echo "  install $2"; return; fi
    mkdir -p "$(dirname "$2")"
    cp "$1" "$2"
}

if [ "$UNINSTALL" = 1 ]; then
    [ "$DRY" = 1 ] && { echo "--dry-run + --uninstall: would remove bins, sounds, desktop file, icon, autostart"; exit 0; }
    for f in "$BINDIR/clicky" "$BINDIR/clicky-overlay" "$APPS/clicky.desktop" \
             "$ICONS/clicky.svg"; do
        rm -f "$f" && echo "removed $f"
    done
    rm -rf "$SHARE/sounds"
    rm -f "$HOME/.config/autostart/clicky.desktop"
    echo "removed $SHARE/sounds and autostart entry"
    echo "(udev rule left in place: remove $RULE_DST manually if desired)"
    exit 0
fi

for b in clicky clicky-overlay; do
    [ -x "$BUILD/$b" ] || { echo "missing $BUILD/$b — run: cargo build --release --workspace" >&2; exit 1; }
done

echo "Installing to $PREFIX"
put "$BUILD/clicky"         "$BINDIR/clicky"
put "$BUILD/clicky-overlay" "$BINDIR/clicky-overlay"
put "$ROOT/packaging/clicky.desktop" "$APPS/clicky.desktop"
put "$ROOT/packaging/clicky.svg"     "$ICONS/clicky.svg"
if [ "$DRY" = 1 ]; then
    echo "  copy sounds/ -> $SHARE/sounds/ ($(find "$ROOT/sounds" -name '*.wav' | wc -l | tr -d ' ') wavs + profiles.json + LICENSES)"
else
    mkdir -p "$SHARE"
    rm -rf "$SHARE/sounds"
    cp -r "$ROOT/sounds" "$SHARE/sounds"
fi

echo
echo "Keyboard access needs the udev uaccess rule (one-time, needs sudo):"
echo "  sudo cp $RULE $RULE_DST"
echo "  sudo udevadm control --reload && sudo udevadm trigger"
echo "  # 'trigger' applies it to already-plugged keyboards — no replug/relogin."
if [ "$DO_UDEV" = 1 ] && [ "$DRY" = 0 ]; then
    sudo cp "$RULE" "$RULE_DST" && sudo udevadm control --reload && sudo udevadm trigger
    echo "udev rule installed + triggered — keyboards are live now (check getfacl)."
fi

echo
echo "Done. Run 'clicky' for the settings app or 'clicky --daemon' headless."
echo "Make sure $BINDIR is on PATH (fish: fish_add_path $BINDIR)."
