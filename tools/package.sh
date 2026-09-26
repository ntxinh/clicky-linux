#!/bin/sh
# Build the clicky RPM into dist/ (needs cargo-generate-rpm:
# `cargo install cargo-generate-rpm` — no rpmbuild/spec file required).
# RPM layout: /usr/bin/{clicky,clicky-overlay}, sounds + desktop + icon +
# udev rule under the usual system paths; post_install reloads udevd.
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
cargo build --release --workspace
strip -s target/release/clicky target/release/clicky-overlay 2>/dev/null || true
cargo generate-rpm -p crates/clicky

mkdir -p dist
cp target/generate-rpm/clicky-*.rpm dist/
echo "→ dist/:"
ls -l dist/clicky-*.rpm
