#!/usr/bin/env bash
# Install or update the Rust Lucina on nezopt (user units, no root):
#   deploy/lucina-rs/install.sh
# - builds and installs target/release/lucina as ~/.local/bin/lucina-rs (previous kept as .prev;
#   the running lucina-presence watcher keeps its old binary until its next restart);
# - installs lucina-rs.service and the health timer, the watchdog and ~/.local/bin/lucina-maint,
#   then restarts lucina-rs and checks /health.
# Config is not generated here: ~/.config/lucina-rs/lucina.env (frozen from the retired Node Lucina,
# now canonical) and mode.env must exist; drop-ins in lucina-rs.service.d (Infisical, LAN, Gemini)
# are left as they are. 90-stop.conf is now the same TimeoutStopSec/KillMode the unit carries, and can
# be removed by hand. The Node container lucina-nez is gone (docs/lucina-rust/RETIREMENT.md).
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
# The shopping catalog Lucina reads first (data/shopping-catalog.v1.json, crates/lucina-grocery/src/catalog.rs)
# is this commit's assets/catalog/catalog.v1.json: a copy left from an older deploy made Red Bull unknown to
# Lucina while the apps already had it (2026-11-08, docs/design/shopping-labels.md). Atomic, re-read within 30 s.
CATALOG=/home/nez/Projects/Lucina/data/shopping-catalog.v1.json
install -m 0664 "$REPO/assets/catalog/catalog.v1.json" "$CATALOG.new"
mv -f "$CATALOG.new" "$CATALOG"

install -m 0644 "$REPO/deploy/lucina-rs.service" "$REPO/deploy/lucina-rs-health.service" "$REPO/deploy/lucina-rs-health.timer" "$UNITS/"
# The watchdog and the maintenance marker are part of this deploy too: nezopt runs the copies in
# ~/.local/bin and nothing else installs them.
install -m 0755 "$REPO/deploy/lucina-rs/health-check.sh" "$HOME/.local/bin/lucina-rs-health-check"
install -m 0755 "$REPO/deploy/lucina-maint" "$HOME/.local/bin/lucina-maint"
# The upstream watch (docs/upstream-watch.md): `lucina-rs upstream check` weekly, `reconcile` hourly.
install -m 0644 "$REPO"/crates/lucina-upstream/deploy/lucina-upstream-{watch,reconcile}.{service,timer} "$UNITS/"
systemctl --user daemon-reload
systemctl --user enable lucina-rs.service lucina-rs-health.timer >/dev/null
systemctl --user enable --now lucina-upstream-watch.timer lucina-upstream-reconcile.timer >/dev/null
# The unit's ExecStartPost waits for /health, so a failed start fails here. `restart` does not fight
# the unit's Restart=always: systemd queues no start for a stop it was asked for, and a restart job is
# exactly that suppressed stop plus one start, so the unit ends with one new main process. That is why
# this deploy needs no maintenance marker: a marker is for staying down across a watchdog tick, and a
# restart is never down between ticks.
systemctl --user restart lucina-rs.service
# A deploy overrules an earlier stop on purpose: the marker would otherwise excuse the next unexpected
# exit until it expires. The unit is up here, so `start` only clears the marker.
"$HOME/.local/bin/lucina-maint" start lucina-rs.service >/dev/null
systemctl --user start lucina-rs-health.timer
systemctl --user --no-pager --lines=0 status lucina-rs.service | head -5
curl -fsS -m 5 http://127.0.0.1:8790/health | head -c 600; echo
