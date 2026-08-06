#!/usr/bin/env bash
# Cross-compile all release binaries into ./dist using the official
# cargo-zigbuild docker image (bundles zig + cargo-zigbuild + all Rust targets).
#
#   linux   x86_64 + arm64 (glibc)
#   windows x86_64 + arm64
#   macos   x86_64 + arm64  -- needs an Apple SDK; build on a Mac / macOS CI
#
# Requires: docker. Artifacts land in ./dist; a named docker volume caches the
# cargo registry so repeat runs don't re-download everything.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
DIST="$ROOT/dist"
mkdir -p "$DIST"

IMG="${ZIGBUILD_IMAGE:-ghcr.io/rust-cross/cargo-zigbuild:latest}"
CARGO_VOL="${ZIGBUILD_CARGO_VOL:-coderadio-cargo-home}"

docker image inspect "$IMG" >/dev/null 2>&1 || docker pull "$IMG"

# run_in <target> <setup+env-prefix>  -> executes `cargo zigbuild --release --target T` after prefix.
run_in() {
  local triple="$1" prefix="$2"
  echo "==> $triple"
  docker run --rm -u root \
    -v "$CARGO_VOL:/usr/local/cargo/registry" \
    -v "$ROOT/target:/workspace/target" \
    -v "$ROOT:/workspace" -w /workspace \
    -e CARGO_TARGET_DIR=/workspace/target \
    "$IMG" bash -lc "export PATH=/usr/local/cargo/bin:\\$PATH; $prefix cargo zigbuild --release --target $triple"
}

# ALSA (needed by cpal on Linux). Distro libasound references a newer glibc
# than zig bundles, so --allow-shlib-undefined keeps the dynamic link happy.
ALLOW_UNDEF="export PKG_CONFIG_ALLOW_SYSTEM_LIBS=1 RUSTFLAGS='-C link-arg=-Wl,--allow-shlib-undefined';"

run_in x86_64-unknown-linux-gnu \
  "set -e; apt-get update -qq; DEBIAN_FRONTEND=noninteractive apt-get install -y -qq libasound2-dev pkg-config; $ALLOW_UNDEF"
cp target/x86_64-unknown-linux-gnu/release/coderadio "$DIST/coderadio-linux-x86_64"

run_in aarch64-unknown-linux-gnu \
  "set -e; dpkg --add-architecture arm64 >/dev/null 2>&1 || true; apt-get update -qq; DEBIAN_FRONTEND=noninteractive apt-get install -y -qq libasound2-dev:arm64 pkg-config; export PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig PKG_CONFIG_SYSROOT_DIR=/ ; $ALLOW_UNDEF"
cp target/aarch64-unknown-linux-gnu/release/coderadio "$DIST/coderadio-linux-arm64"

run_in x86_64-pc-windows-gnu "set -e;"
cp target/x86_64-pc-windows-gnu/release/coderadio.exe "$DIST/coderadio-windows-x86_64.exe"

run_in aarch64-pc-windows-gnullvm "set -e;"
cp target/aarch64-pc-windows-gnullvm/release/coderadio.exe "$DIST/coderadio-windows-arm64.exe"

# macOS targets need an Apple SDK (CoreAudio/CoreFoundation framework stubs).
echo "==> skipping macOS (set SDKROOT to a MacOSX.sdk, or build on a Mac / macOS CI runner)"

echo
echo "done. artifacts in $DIST:"
ls -la "$DIST"