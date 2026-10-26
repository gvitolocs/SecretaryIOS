#!/bin/bash
# Full parity + verification pipeline for the NClientV3 Rust port.
# Usage: ./run_all.sh   (from anywhere; runs on the local machine)
set -euo pipefail
cd "$(dirname "$0")"

echo "== 1. Java golden generation (JVM runs the ORIGINAL NClientV3 logic) =="
( cd java-goldens && javac JavaGolden.java && java JavaGolden goldens.json )

echo "== 2. Rust build + golden parity tests =="
cargo test -p nclient-core

echo "== 3. Verus verification of core invariants =="
VERUS_BIN="${VERUS_BIN:-$HOME/tools/verus/verus-x86-linux/verus}"
[ -x "$VERUS_BIN" ] || VERUS_BIN="$HOME/.cache/verus/verus-arm64-macos/verus"
"$VERUS_BIN" verus/verify_core.rs

echo "== ALL GREEN: goldens regenerated, parity 13/13, Verus 0 errors =="
