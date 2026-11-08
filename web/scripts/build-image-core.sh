#!/bin/sh
# Builds the photo viewer core (crates/secretary-image-wasm) and copies it next to its loader,
# web/src/lib/image-core.wasm (committed, so web builds need no Rust toolchain). Run it
# after changing crates/secretary-image; imageCore.test.ts checks the committed module.
set -eu
root="$(cd "$(dirname "$0")/../.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
target="${CARGO_TARGET_DIR:-$root/target}"
cd "$root"
cargo build --release --locked --target wasm32-unknown-unknown -p secretary-image-wasm
cp "$target/wasm32-unknown-unknown/release/secretary_image_wasm.wasm" web/src/lib/image-core.wasm
ls -l web/src/lib/image-core.wasm
