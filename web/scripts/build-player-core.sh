#!/bin/sh
# Builds the player core (crates/secretary-player-wasm) and copies it next to its loader,
# web/src/lib/player-core.wasm (committed, so web builds need no Rust toolchain). Run it
# after changing crates/secretary-player; playerCore.test.ts checks the committed module.
set -eu
root="$(cd "$(dirname "$0")/../.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
target="${CARGO_TARGET_DIR:-$root/target}"
cd "$root"
cargo build --release --locked --target wasm32-unknown-unknown -p secretary-player-wasm
cp "$target/wasm32-unknown-unknown/release/secretary_player_wasm.wasm" web/src/lib/player-core.wasm
ls -l web/src/lib/player-core.wasm
