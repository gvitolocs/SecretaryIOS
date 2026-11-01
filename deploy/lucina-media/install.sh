#!/usr/bin/env bash
# Install lucina-media on nezopt from this checkout (what `lagent deploy lucina-media` runs, after deploy-guard check).
# Builds it, keeps the running binary as ~/.local/bin/lucina-media.bak-<UTC stamp>-pre-<sha8>, restarts the unit and
# checks /healthz. The unit and its drop-ins (Infisical) are left as they are.
set -euo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$REPO/target}
BIN=$HOME/.local/bin/lucina-media
SHA=$(git -C "$REPO" rev-parse --short=8 HEAD)
STAMP=$(date -u +%Y%m%dT%H%M%SZ)

(cd "$REPO" && cargo build --release -p lucina-media)
[ -f "$BIN" ] && cp -p "$BIN" "$BIN.bak-$STAMP-pre-$SHA"
install -m 0755 "$TARGET/release/lucina-media" "$BIN.new"
mv -f "$BIN.new" "$BIN"
systemctl --user restart lucina-media.service
for _ in $(seq 1 30); do
  if curl -fsS -m 3 http://127.0.0.1:8830/healthz >/dev/null 2>&1; then
    echo "installed lucina-media at $SHA (previous: $BIN.bak-$STAMP-pre-$SHA)"
    exit 0
  fi
  sleep 1
done
echo "lucina-media did not answer /healthz within 30 s; roll back: cp -p $BIN.bak-$STAMP-pre-$SHA $BIN && systemctl --user restart lucina-media" >&2
exit 1
