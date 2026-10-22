#!/usr/bin/env bash
# Install or update the Rust Lucina on nezopt (user units, no root):
#   deploy/lucina-rs/install.sh            # shadow mode on first install; keeps the current mode after
# - installs target/release/lucina as ~/.local/bin/lucina-rs (previous kept as .prev;
#   the running lucina-presence watcher keeps its old binary until its next restart);
# - regenerates ~/.config/lucina-rs/node.env from the running lucina-nez (values never printed);
# - installs lucina-rs.service and the health timer, then (re)starts lucina-rs.
set -euo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN=$REPO/target/release/lucina
CFG=$HOME/.config/lucina-rs
UNITS=$HOME/.config/systemd/user
[ -x "$BIN" ] || (cd "$REPO" && cargo build --release -p lucina-server)

install -m 0755 "$BIN" "$HOME/.local/bin/lucina-rs.new"
[ -f "$HOME/.local/bin/lucina-rs" ] && cp -p "$HOME/.local/bin/lucina-rs" "$HOME/.local/bin/lucina-rs.prev"
mv -f "$HOME/.local/bin/lucina-rs.new" "$HOME/.local/bin/lucina-rs"

mkdir -p "$CFG" "$UNITS"; chmod 700 "$CFG"
"$HOME/.local/bin/lucina-rs" env-from-node --container lucina-nez --format systemd \
  --out "$CFG/node.env" --token-out "$CFG/node-secretary.token" | sed -E 's/: .*/ (names in the file)/'
[ -f "$CFG/mode.env" ] || install -m 0600 "$REPO/deploy/lucina-rs/shadow.env" "$CFG/mode.env"
mkdir -p /home/nez/Projects/Lucina/data/lucina-rs

install -m 0644 "$REPO/deploy/lucina-rs.service" "$REPO/deploy/lucina-rs-health.service" "$REPO/deploy/lucina-rs-health.timer" "$UNITS/"
systemctl --user daemon-reload
systemctl --user enable lucina-rs.service lucina-rs-health.timer >/dev/null
systemctl --user restart lucina-rs.service
systemctl --user start lucina-rs-health.timer
systemctl --user --no-pager --lines=0 status lucina-rs.service | head -5
curl -fsS http://127.0.0.1:8790/health | head -c 600; echo
