#!/usr/bin/env bash
# Install or update the Rust Lucina on nezopt (user units, no root):
#   deploy/lucina-rs/install.sh
# - builds and installs target/release/lucina as ~/.local/bin/lucina-rs (previous kept as .prev;
#   the running lucina-presence watcher keeps its old binary until its next restart);
# - installs lucina-rs.service and the health timer, then restarts lucina-rs and checks /health.
# Config is not generated here: ~/.config/lucina-rs/lucina.env (frozen from the retired Node Lucina,
# now canonical) and mode.env must exist; drop-ins in lucina-rs.service.d (Infisical, LAN, Gemini)
# are left as they are. The Node container lucina-nez is gone (docs/lucina-rust/RETIREMENT.md).
set -euo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$REPO/target}
BIN=$TARGET/release/lucina
CFG=$HOME/.config/lucina-rs
UNITS=$HOME/.config/systemd/user

for f in lucina.env mode.env; do
  [ -s "$CFG/$f" ] || { echo "install.sh: $CFG/$f is missing; restore it from its .bak-* copy first" >&2; exit 1; }
done
# Always build: cargo is incremental, and an old binary must never be installed by mistake.
(cd "$REPO" && cargo build --release -p lucina-server)

install -m 0755 "$BIN" "$HOME/.local/bin/lucina-rs.new"
[ -f "$HOME/.local/bin/lucina-rs" ] && cp -p "$HOME/.local/bin/lucina-rs" "$HOME/.local/bin/lucina-rs.prev"
mv -f "$HOME/.local/bin/lucina-rs.new" "$HOME/.local/bin/lucina-rs"
mkdir -p "$UNITS" /home/nez/Projects/Lucina/data/lucina-rs

install -m 0644 "$REPO/deploy/lucina-rs.service" "$REPO/deploy/lucina-rs-health.service" "$REPO/deploy/lucina-rs-health.timer" "$UNITS/"
# The upstream watch (docs/upstream-watch.md): `lucina-rs upstream check` weekly, `reconcile` hourly.
install -m 0644 "$REPO"/crates/lucina-upstream/deploy/lucina-upstream-{watch,reconcile}.{service,timer} "$UNITS/"
systemctl --user daemon-reload
systemctl --user enable lucina-rs.service lucina-rs-health.timer >/dev/null
systemctl --user enable --now lucina-upstream-watch.timer lucina-upstream-reconcile.timer >/dev/null
# The unit's ExecStartPost waits for /health, so a failed start fails here.
systemctl --user restart lucina-rs.service
systemctl --user start lucina-rs-health.timer
systemctl --user --no-pager --lines=0 status lucina-rs.service | head -5
curl -fsS -m 5 http://127.0.0.1:8790/health | head -c 600; echo
