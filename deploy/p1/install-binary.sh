#!/usr/bin/env bash
# Install a static p1 binary built from this checkout (what `lagent deploy secretary-server|secretary-mcp` runs,
# after deploy-guard check; the recipe's build step made it):
#   deploy/p1/install-binary.sh secretary-server|secretary-mcp
# Keeps the running binary as /usr/local/bin/<name>.bak-<UTC stamp>-pre-<sha8> on p1, restarts the unit, checks it.
set -euo pipefail
NAME=${1:?usage: install-binary.sh secretary-server|secretary-mcp}
REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN=${CARGO_TARGET_DIR:-$REPO/target}/x86_64-unknown-linux-musl/release/$NAME
SHA=$(git -C "$REPO" rev-parse --short=8 HEAD)
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
case "$NAME" in
  secretary-server) CHECK='curl -fsS -m 5 http://127.0.0.1:8800/v1/health | head -c 300; echo' ;;
  secretary-mcp) CHECK='code=$(curl -s -o /dev/null -w "%{http_code}" -m 5 http://127.0.0.1:8810/mcp); echo "mcp answers $code"; [ "$code" != 000 ]' ;;
  *) echo "install-binary.sh: unknown runtime $NAME" >&2; exit 2 ;;
esac
test -x "$BIN"
scp -q "$BIN" "p1:/tmp/$NAME.new" </dev/null
ssh p1 "set -e
sudo -n cp -p /usr/local/bin/$NAME /usr/local/bin/$NAME.bak-$STAMP-pre-$SHA
sudo -n install -m 0755 /tmp/$NAME.new /usr/local/bin/$NAME
rm -f /tmp/$NAME.new
sudo -n systemctl restart $NAME
sleep 4
systemctl is-active $NAME
$CHECK" </dev/null
echo "installed $NAME at $SHA (previous: /usr/local/bin/$NAME.bak-$STAMP-pre-$SHA on p1)"
