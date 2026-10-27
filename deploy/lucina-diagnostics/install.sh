#!/usr/bin/env bash
# Install or update lucina-diagnostics on nezopt (user unit, no root):
#   deploy/lucina-diagnostics/install.sh
# - builds and installs target/release/lucina-diagnostics as ~/.local/bin/lucina-diagnostics (previous kept as .prev);
# - creates ~/.config/lucina-diagnostics/{diagnostics.env,gateway.token,service.token} on first install;
# - installs lucina-diagnostics.service and restarts it.
# The env template binds 127.0.0.1 and nezopt's tailnet address (p1's Caddy proxies /v1/diagnostics there,
# secretary-mcp on p1 reads it there). Then: the p1 Caddy block and the secretary-mcp settings in docs/diagnostics.md.
set -euo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$REPO/target}
CFG=$HOME/.config/lucina-diagnostics
UNITS=$HOME/.config/systemd/user
DATA=$HOME/data/lucina-diagnostics
(cd "$REPO" && cargo build --release -p lucina-diagnostics)
install -m 0755 "$TARGET/release/lucina-diagnostics" "$HOME/.local/bin/lucina-diagnostics.new"
[ -f "$HOME/.local/bin/lucina-diagnostics" ] && cp -p "$HOME/.local/bin/lucina-diagnostics" "$HOME/.local/bin/lucina-diagnostics.prev"
mv -f "$HOME/.local/bin/lucina-diagnostics.new" "$HOME/.local/bin/lucina-diagnostics"

mkdir -p "$CFG" "$UNITS" "$DATA"; chmod 700 "$CFG" "$DATA"
( umask 077
  for t in gateway service; do
    [ -s "$CFG/$t.token" ] || head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$CFG/$t.token"
  done
  [ -f "$CFG/diagnostics.env" ] || sed "s|@TAILNET@|$(tailscale ip -4 | head -1)|" "$REPO/deploy/lucina-diagnostics/diagnostics.env.example" > "$CFG/diagnostics.env" )

install -m 0644 "$REPO/deploy/lucina-diagnostics/lucina-diagnostics.service" "$UNITS/"
systemctl --user daemon-reload
systemctl --user enable lucina-diagnostics.service >/dev/null
systemctl --user restart lucina-diagnostics.service
systemctl --user --no-pager --lines=0 status lucina-diagnostics.service | head -5
curl -fsS http://127.0.0.1:8850/health; echo
