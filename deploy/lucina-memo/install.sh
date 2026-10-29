#!/usr/bin/env bash
# Install or update lucina-memo on nezopt (user unit, no root):
#   deploy/lucina-memo/install.sh
# - builds and installs target/release/lucina-memo as ~/.local/bin/lucina-memo (previous kept as .prev);
# - creates ~/.config/lucina-memo/{memo.env,gateway.token,service.token} on first install;
# - installs lucina-memo.service and restarts it.
# The memo.env template binds 127.0.0.1 and nezopt's tailnet address (p1's Caddy proxies /v1/memos there).
set -euo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$REPO/target}
CFG=$HOME/.config/lucina-memo
UNITS=$HOME/.config/systemd/user
(cd "$REPO" && cargo build --release -p lucina-memo)
install -m 0755 "$TARGET/release/lucina-memo" "$HOME/.local/bin/lucina-memo.new"
[ -f "$HOME/.local/bin/lucina-memo" ] && cp -p "$HOME/.local/bin/lucina-memo" "$HOME/.local/bin/lucina-memo.prev"
mv -f "$HOME/.local/bin/lucina-memo.new" "$HOME/.local/bin/lucina-memo"

mkdir -p "$CFG" "$UNITS" "$HOME/Projects/Lucina/data/memos"; chmod 700 "$CFG" "$HOME/Projects/Lucina/data/memos"
( umask 077
  for t in gateway service; do
    [ -s "$CFG/$t.token" ] || head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$CFG/$t.token"
  done
  [ -f "$CFG/memo.env" ] || sed "s|@TAILNET@|$(tailscale ip -4 | head -1)|" "$REPO/deploy/lucina-memo/memo.env.example" > "$CFG/memo.env" )

# The meetings archive lives on the MyBook; create it only when the disk is mounted (never on
# the system disk under an empty mount point).
if mountpoint -q "$HOME/mnt/mybook"; then
  mkdir -p "$HOME/mnt/mybook/lucina-meetings" && chmod 700 "$HOME/mnt/mybook/lucina-meetings"
else
  echo "note: $HOME/mnt/mybook is not mounted; meetings answer 503 until $HOME/mnt/mybook/lucina-meetings exists" >&2
fi
[ -f "$HOME/models/diarization/onnxruntime/lib/libonnxruntime.so" ] || echo "note: diarization models/ONNX Runtime missing (docs/meetings.md, one-time download)" >&2

install -m 0644 "$REPO/deploy/lucina-memo/lucina-memo.service" "$UNITS/"
# DeepSeek key from infisical-brokerd (caller svc-lucina-memo in /etc/infisical-broker/broker.toml).
if [ -s "$HOME/.config/infisical-mcp/svc-lucina-memo.token" ]; then
  install -D -m 0644 "$REPO/deploy/lucina-memo/70-infisical.conf" "$UNITS/lucina-memo.service.d/70-infisical.conf"
else
  echo "note: no ~/.config/infisical-mcp/svc-lucina-memo.token; summaries and briefs fall back (no DeepSeek key)" >&2
fi
install -m 0755 "$REPO/deploy/lucina-memo/lucina-memo-backup.sh" "$HOME/.local/bin/lucina-memo-backup"
install -m 0644 "$REPO/deploy/lucina-memo/lucina-memo-backup.service" "$REPO/deploy/lucina-memo/lucina-memo-backup.timer" "$UNITS/"
systemctl --user daemon-reload
systemctl --user enable --now lucina-memo-backup.timer
systemctl --user enable lucina-memo.service >/dev/null
systemctl --user restart lucina-memo.service
systemctl --user --no-pager --lines=0 status lucina-memo.service | head -5
curl -fsS http://127.0.0.1:8795/health; echo
