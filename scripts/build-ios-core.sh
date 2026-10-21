#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
export PATH="$HOME/.cargo/bin:$PATH"

ios_target="aarch64-apple-ios"
sim_target="aarch64-apple-ios-sim"
out="${SECRETARY_XCFRAMEWORK_OUT:-$root/ios/Frameworks/SecretaryCore.xcframework}"
target_dir="${CARGO_TARGET_DIR:-$root/target}"

if ! command -v xcodebuild >/dev/null 2>&1; then
    echo "error: xcodebuild not found; the iOS core can only be built on macOS with Xcode installed" >&2
    exit 1
fi

rustup target add "$ios_target" "$sim_target"

cargo build --locked --release -p secretary-ffi --target "$ios_target"
cargo build --locked --release -p secretary-ffi --target "$sim_target"

headers="$(mktemp -d "${TMPDIR:-/tmp}/secretary-headers.XXXXXX")"
trap 'rm -rf "$headers"' EXIT
cp crates/secretary-ffi/include/SecretaryCore.h "$headers/SecretaryCore.h"
cat > "$headers/module.modulemap" <<'MODULEMAP'
module SecretaryCore {
    header "SecretaryCore.h"
    export *
}
MODULEMAP

rm -rf "$out"
mkdir -p "$(dirname "$out")"
xcodebuild -create-xcframework \
    -library "$target_dir/$ios_target/release/libsecretary_ffi.a" -headers "$headers" \
    -library "$target_dir/$sim_target/release/libsecretary_ffi.a" -headers "$headers" \
    -output "$out"

echo "created $out"
