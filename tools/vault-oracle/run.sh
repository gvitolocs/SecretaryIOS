#!/usr/bin/env bash
# run.sh <vault> <out.json> [script.js]: real Obsidian, headless and CPU-capped, on a throwaway copy of <vault>.
# OBSIDIAN=<path to an extracted AppImage's obsidian binary>. Nothing is left running or written outside $work.
set -euo pipefail
vault=$(realpath "$1")
out=$(realpath -m "$2")
script=${3:-}
port=${ORACLE_PORT:-9333}
: "${OBSIDIAN:?extracted Obsidian binary}"
tool=$(dirname "$(realpath "$0")")
work=$(mktemp -d "${TMPDIR:-$HOME/scratch}/vault-oracle.XXXXXX")
trap '[ $? = 0 ] || tail -5 "$work/obsidian.log" >&2; pkill -f "[r]emote-debugging-port=$port" || true; chmod -R u+w "$work"; rm -rf "$work"' EXIT
cp -a "$vault" "$work/vault"
chmod -R u+w "$work/vault"
mkdir -p "$work/cfg/obsidian"
printf '{"vaults":{"0123456789abcdef":{"path":"%s","ts":1790000000000,"open":true}}}\n' "$work/vault" >"$work/cfg/obsidian/obsidian.json"
XDG_CONFIG_HOME="$work/cfg" HOME="$work" systemd-run --user --scope --quiet -p CPUQuota=300% \
    xvfb-run -a "$OBSIDIAN" --no-sandbox --disable-gpu --remote-debugging-port="$port" --remote-allow-origins='*' \
    >"$work/obsidian.log" 2>&1 &
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/cargo-target/vault-oracle}" \
    cargo run -q --release --manifest-path "$tool/Cargo.toml" -- "$port" "$out" ${script:+"$(realpath "$script")"}
