#!/usr/bin/env bash
# Runs the OTP differential tests (verification/verus/diff/otp_*.rs) against the real
# crates of REV (default lucina-rs, which has F1-F5 since dd7ec9e), without changing it:
#   1. `git archive` lucina-phone-core, lucina-otp-relay and lucina-android (+ Cargo.lock)
#      from REV (default lucina-rs) into a scratch workspace with a 3-member Cargo.toml;
#   2. installs the tests where they belong:
#        otp_tokenizer.rs -> crates/lucina-phone-core/tests/otp_common/mod.rs
#        otp_extract.rs   -> crates/lucina-phone-core/tests/verus_otp_extract.rs
#        otp_redact.rs    -> crates/lucina-phone-core/tests/verus_otp_redact.rs
#        otp_owner.rs     -> crates/lucina-phone-core/tests/verus_otp_owner.rs
#        otp_expiry.rs    -> crates/lucina-otp-relay/tests/verus_otp_store.rs
#        otp_queue.rs     -> crates/lucina-android/src/verus_otp_queue.rs (+ a mod line in lib.rs)
#   3. cargo test of those targets (`--ignored` also runs #[ignore]d tests; there are none now).
# No new dependency is added (the generators are a built-in xorshift).
# On nezopt run it through the memory-capped gate:
#   ~/.local/bin/verify-gate -- verification/verus/diff/otp_run.sh
# Env: REV (default lucina-rs), OTP_DIFF_DIR (default ~/scratch/otp-diff), CARGO_JOBS (default 2).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"
rev="${REV:-lucina-rs}"
dir="${OTP_DIFF_DIR:-$HOME/scratch/otp-diff}"
ignored=()
[ "${1:-}" = "--ignored" ] && ignored=(-- --include-ignored)

mkdir -p "$dir"
rm -rf "$dir/crates"
git -C "$repo" archive "$rev" crates/lucina-phone-core crates/lucina-otp-relay crates/lucina-android Cargo.lock \
    | tar -x -C "$dir"
cat >"$dir/Cargo.toml" <<'EOF'
[workspace]
resolver = "2"
members = ["crates/lucina-phone-core", "crates/lucina-otp-relay", "crates/lucina-android"]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
EOF
core="$dir/crates/lucina-phone-core/tests"
mkdir -p "$core/otp_common" "$dir/crates/lucina-otp-relay/tests"
cp "$here/otp_tokenizer.rs" "$core/otp_common/mod.rs"
cp "$here/otp_extract.rs" "$core/verus_otp_extract.rs"
cp "$here/otp_redact.rs" "$core/verus_otp_redact.rs"
cp "$here/otp_owner.rs" "$core/verus_otp_owner.rs"
cp "$here/otp_expiry.rs" "$dir/crates/lucina-otp-relay/tests/verus_otp_store.rs"
cp "$here/otp_queue.rs" "$dir/crates/lucina-android/src/verus_otp_queue.rs"
printf '\n#[cfg(test)]\nmod verus_otp_queue;\n' >>"$dir/crates/lucina-android/src/lib.rs"

echo "otp diff: $rev $(git -C "$repo" rev-parse --short "$rev") in $dir"
cd "$dir"
export CARGO_TARGET_DIR="$dir/target"
j="${CARGO_JOBS:-2}"
cargo test -j"$j" -q -p lucina-phone-core --test verus_otp_extract --test verus_otp_redact --test verus_otp_owner "${ignored[@]}"
cargo test -j"$j" -q -p lucina-otp-relay --test verus_otp_store "${ignored[@]}"
cargo test -j"$j" -q -p lucina-android --lib verus_otp_queue "${ignored[@]}"
echo "otp diff: all passed"
