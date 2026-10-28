#!/usr/bin/env bash
set -euo pipefail
umask 077
mountpoint -q /home/nez/mnt/mybook
export LUCINA_MEMO_ARCHIVE_DIR=${LUCINA_MEMO_ARCHIVE_DIR:-/home/nez/mnt/mybook/lucina-memos}
"$HOME/.local/bin/lucina-memo" archive --all --verify
snapshots=/home/nez/mnt/mybook/backups/lucina-memos
today=$(date +%F)
previous=$(date -d yesterday +%F)
mkdir -p "$snapshots"
args=()
if [[ -d "$snapshots/$previous" ]]; then
    args+=(--link-dest="$snapshots/$previous")
fi
rsync -a "${args[@]}" "$LUCINA_MEMO_ARCHIVE_DIR/" "$snapshots/$today/"
# Delete only dated snapshot directories older than the retention window.
cutoff=$(date -d '60 days ago' +%F)
for snapshot in "$snapshots"/????-??-??; do
    [[ -d "$snapshot" && ! -L "$snapshot" ]] || continue
    name=${snapshot##*/}
    [[ "$name" < "$cutoff" || "$name" == "$cutoff" ]] && rm -rf -- "$snapshot"
done
exit 0
