#!/usr/bin/env bash
# Differential tests of the Verus deals kernel against the real lucina-deals code.
#
# The code is read from the pinned commit in deals-revision with `git archive` into
# target/deals-diff (no checkout, the lucina-rs branch is never touched). The test
# files are installed next to the extracted crate:
#   deals.rs              -> crates/lucina-deals/tests/verus_deals.rs
#   deals_kernel_exec.rs  -> crates/lucina-deals/tests/verus/deals_kernel_exec.rs
#   deals_group.rs        -> crates/lucina-deals/tests/verus/group.rs, included by a
#                            test module appended to the extracted engine.rs (for the
#                            private group_deals; no existing line changes)
# deals_kernel_exec.rs is a plain-Rust copy of the kernel's exec functions; the run
# stops when deals_kernel.rs changed since that copy was checked (deals-kernel.sha256).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
cd "$root"

want="$(cut -d' ' -f1 "$here/deals-kernel.sha256")"
have="$(sha256sum verification/verus/deals_kernel.rs | cut -d' ' -f1)"
if [ "$want" != "$have" ]; then
    echo "deals_kernel.rs changed (sha256 $have, the plain-Rust copy was checked against $want):" >&2
    echo "update verification/verus/diff/deals_kernel_exec.rs, then deals-kernel.sha256" >&2
    exit 1
fi

rev="$(cat "$here/deals-revision")"
git cat-file -e "$rev^{commit}"
snap="$root/target/deals-diff"
rm -rf "$snap"
mkdir -p "$snap"
git archive "$rev" crates/lucina-deals crates/lucina-grocery Cargo.lock | tar -x -C "$snap"

# A two-member workspace with the pinned workspace.package / workspace.dependencies.
{
    printf '[workspace]\nresolver = "2"\nmembers = ["crates/lucina-deals", "crates/lucina-grocery"]\n\n'
    git show "$rev:Cargo.toml" | awk '/^\[workspace\.(package|dependencies)\]/ {on = 1; print; next} /^\[/ {on = 0} on'
} > "$snap/Cargo.toml"

crate="$snap/crates/lucina-deals"
mkdir -p "$crate/tests/verus"
cp "$here/deals.rs" "$crate/tests/verus_deals.rs"
cp "$here/deals_kernel_exec.rs" "$crate/tests/verus/deals_kernel_exec.rs"
cp "$here/deals_group.rs" "$crate/tests/verus/group.rs"
cat >> "$crate/src/engine.rs" <<'RUST'

#[cfg(test)]
mod verus_group {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/verus/group.rs"));
}
RUST

jobs="${CARGO_JOBS:-2}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"
cargo test -q -j"$jobs" --manifest-path "$snap/Cargo.toml" -p lucina-deals --test verus_deals
cargo test -q -j"$jobs" --manifest-path "$snap/Cargo.toml" -p lucina-deals --lib verus_group
