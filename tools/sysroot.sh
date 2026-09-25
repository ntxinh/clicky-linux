#!/usr/bin/env bash
# Populate .sysroot/ with -devel headers+libs downloaded as RPMs (no sudo).
# Usage: tools/sysroot.sh [pkg ...]   (default: alsa-lib-devel libudev-devel)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SYS="$ROOT/.sysroot"
PKGS=("${@:-alsa-lib-devel libudev-devel}")
mkdir -p "$SYS/dl" "$SYS"
cd "$SYS/dl"
for p in "${PKGS[@]}"; do dnf download --quiet --arch=x86_64 --destdir="$SYS/dl" "$p"; done
for r in "$SYS/dl"/*.rpm; do (cd "$SYS" && rpm2cpio "$r" | cpio -idmu --quiet); done
# .pc files point at /usr — rewrite to absolute sysroot paths
for pc in "$SYS"/usr/lib64/pkgconfig/*.pc; do
  sed -i "s|^prefix=/usr$|prefix=$SYS/usr|; s|^libdir=/usr/lib64$|libdir=$SYS/usr/lib64|; s|^includedir=/usr/include$|includedir=$SYS/usr/include|" "$pc"
done
# dev symlinks (libfoo.so -> libfoo.so.N) need the real runtime libs they name —
# copy them from the system so links resolve
for so in "$SYS"/usr/lib64/*.so; do
  tgt="$(readlink "$so" 2>/dev/null || true)"; [ -z "$tgt" ] && continue
  [ -e "$SYS/usr/lib64/$tgt" ] || cp "/usr/lib64/$tgt" "$SYS/usr/lib64/" 2>/dev/null || cp "/lib64/$tgt" "$SYS/usr/lib64/" 2>/dev/null || true
done
echo "sysroot at $SYS (PKG_CONFIG_PATH is wired via .cargo/config.toml)"
