#!/usr/bin/env bash
# Populate .sysroot/ with -devel headers+libs downloaded as RPMs (no sudo).
# Usage: tools/sysroot.sh           (default set below)
#        tools/sysroot.sh pkg ...   (extra/override packages)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SYS="$ROOT/.sysroot"
DEFAULT="alsa-lib-devel libudev-devel webkit2gtk4.1-devel gtk3-devel libsoup3-devel gtk4-devel gtk4-layer-shell-devel"
PKGS=("${@:-$DEFAULT}")
mkdir -p "$SYS/dl"
cd "$SYS/dl"
# --resolve pulls the full transitive closure of -devel deps
dnf download --quiet --arch=x86_64 --resolve --destdir="$SYS/dl" ${PKGS[@]/#/--resolve=}
for r in "$SYS/dl"/*.rpm; do (cd "$SYS" && rpm2cpio "$r" | cpio -idmu --quiet); done
# .pc files point at /usr — rewrite to absolute sysroot paths
for pc in "$SYS"/usr/lib64/pkgconfig/*.pc "$SYS"/usr/share/pkgconfig/*.pc; do
  [ -f "$pc" ] && sed -i "s|^prefix=/usr$|prefix=$SYS/usr|; s|^libdir=/usr/lib64\$|libdir=$SYS/usr/lib64|; s|^includedir=/usr/include\$|includedir=$SYS/usr/include|; s|^datadir=/usr/share\$|datadir=$SYS/usr/share|" "$pc"
done
# dev symlinks (libfoo.so -> libfoo.so.N) need the real runtime libs they name
for so in "$SYS"/usr/lib64/*.so; do
  tgt="$(readlink "$so" 2>/dev/null || true)"; [ -z "$tgt" ] && continue
  [ -e "$SYS/usr/lib64/$tgt" ] || cp "/usr/lib64/$tgt" "$SYS/usr/lib64/" 2>/dev/null || cp "/lib64/$tgt" "$SYS/usr/lib64/" 2>/dev/null || true
done
rm -rf "$SYS/dl"
echo "sysroot at $SYS (PKG_CONFIG_PATH wired via .cargo/config.toml)"
