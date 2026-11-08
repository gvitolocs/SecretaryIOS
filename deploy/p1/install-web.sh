#!/usr/bin/env bash
# Install web/dist on p1 (what `lagent deploy web` runs, after deploy-guard check; the recipe's build step made it).
# Keeps the running site as /var/lib/secretary/web.bak-<UTC stamp>-pre-<sha8> on p1.
set -euo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
SHA=$(git -C "$REPO" rev-parse --short=8 HEAD)
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
test -s "$REPO/web/dist/index.html"
rsync -a --delete "$REPO/web/dist/" p1:/tmp/secretary-web/ </dev/null
ssh p1 "set -e
sudo -n cp -a /var/lib/secretary/web /var/lib/secretary/web.bak-$STAMP-pre-$SHA
sudo -n rsync -a --delete --chown=secretary:secretary /tmp/secretary-web/ /var/lib/secretary/web/
sudo -n test -s /var/lib/secretary/web/index.html" </dev/null
echo "installed web at $SHA (previous: /var/lib/secretary/web.bak-$STAMP-pre-$SHA on p1)"
