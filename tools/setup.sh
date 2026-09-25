#!/usr/bin/env bash
# clicky-linux system setup — Fedora 44.
#
# Run it directly from fish or bash:
#   ./tools/setup.sh            print what it would do (no sudo)
#   ./tools/setup.sh --install  actually run the sudo commands
#
# Steps:
#   1. dnf: Tauri + gtk4 + audio + udev build deps
#   2. pnpm via npm -g (UI toolchain, used by Task 14)
#   3. udev uaccess rule → seat-user ACL on keyboard event nodes

set -euo pipefail

RULES_SRC="$(cd "$(dirname "$0")/.." && pwd)/udev/99-clicky-uaccess.rules"
RULES_DST="/etc/udev/rules.d/99-clicky-uaccess.rules"

DNF_PKGS="webkit2gtk4.1-devel openssl-devel gtk4-devel gtk4-layer-shell-devel \
libappindicator-gtk3-devel librsvg2-devel alsa-lib-devel libudev-devel nodejs"

echo "clicky-linux setup"
echo
echo "Will run:"
echo "  sudo dnf install $DNF_PKGS"
echo "  npm i -g pnpm"
echo "  sudo cp $RULES_SRC $RULES_DST"
echo "  sudo udevadm control --reload && sudo udevadm trigger"
echo
echo "Afterwards: log out/in (or replug the keyboard) so the uaccess ACL"
echo "applies, then 'getfacl /dev/input/event*' should list your user."

if [ "${1:-}" != "--install" ]; then
    echo
    echo "Dry run only. Re-run with --install to execute."
    exit 0
fi

sudo dnf install $DNF_PKGS
npm i -g pnpm
sudo cp "$RULES_SRC" "$RULES_DST"
sudo udevadm control --reload
sudo udevadm trigger

echo
echo "Done. After re-login/replug, verify with:"
echo "  getfacl /dev/input/event* | grep \$USER"
echo "Then uncomment the marked deps in crates/*/Cargo.toml."
