#!/usr/bin/env bash
# ONE COMMAND: move the Secretary API (app + web chat, Lucina MCP through
# secretary-server) from the Node Lucina (:8788) to the Rust Lucina (:8790).
# Run from a machine with SSH to nezopt and p1 (the Mac):   deploy/lucina-rs/cutover.sh
# Undo:                                                      deploy/lucina-rs/rollback.sh
#
# What it does:
#   1. Preflight: lucina-rs healthy, Node healthy (the rollback target), p1 reaches :8790.
#   2. nezopt, timeline handover: the live timeline.jsonl becomes owned by nez (Node runs as
#      root and keeps appending Telegram, Discord and reminder items; Rust appends the
#      secretary turns; both write one whole line per O_APPEND write, Rust also takes
#      flock(timeline.jsonl.lock) and moves its ids past the newest line in the file).
#      Then lucina-rs switches from shadow.env to cutover.env: live timeline, Honcho
#      read-write, Harness jobs on; reminders still forwarded to Node (phase 2 moves them).
#   3. p1: SECRETARY_LUCINA_URL -> http://100.107.164.33:8790 (backup kept), restart
#      secretary-server, and check its Lucina probe.
set -euo pipefail
NEZ=${NEZOPT_HOST:-}
if [ -z "$NEZ" ]; then NEZ=nezopt; nc -z -G 2 nezopt.fritz.box 22 >/dev/null 2>&1 || NEZ=nezopt-remote; fi
P1=${P1_HOST:-p1}
NEW=http://100.107.164.33:8790
say() { printf '[cutover] %s\n' "$*"; }

say "preflight on $NEZ"
ssh "$NEZ" 'systemctl --user is-active --quiet lucina-rs.service \
  && curl -fsS -m 5 -o /dev/null http://127.0.0.1:8790/health \
  && curl -fsS -m 5 -o /dev/null http://127.0.0.1:8788/health \
  && docker image inspect alpine:3.20 >/dev/null'
say "preflight on $P1"
ssh "$P1" "curl -fsS -m 6 -o /dev/null $NEW/health && sudo -n grep -q '^SECRETARY_LUCINA_URL=' /etc/secretary/secretary.env"

say "nezopt: timeline handover and live mode"
ssh "$NEZ" bash -s <<'REMOTE'
set -euo pipefail
D=/home/nez/Projects/Lucina/data
for f in timeline.jsonl timeline.jsonl.1 timeline.jsonl.2; do
  [ -e "$D/$f" ] && docker run --rm -v "$D:/d" alpine:3.20 chown "$(id -u):$(id -g)" "/d/$f"
done
touch "$D/timeline.jsonl.lock"
cp -p ~/.config/lucina-rs/mode.env ~/.config/lucina-rs/mode.env.before-cutover
install -m 0600 /home/nez/Projects/SecretaryIOS-lucina/deploy/lucina-rs/cutover.env ~/.config/lucina-rs/mode.env
systemctl --user restart lucina-rs.service
h=$(curl -fsS -m 5 http://127.0.0.1:8790/health)
echo "$h" | grep -q '"shadow":false' && echo "$h" | grep -q 'Lucina/data/timeline.jsonl"' && echo "$h" | grep -q '"mode":"readwrite"' \
  || { echo "lucina-rs did not come up in live mode" >&2; exit 1; }
echo "lucina-rs live: shared timeline, Honcho read-write"
REMOTE

say "p1: secretary-server -> $NEW"
ssh "$P1" "sudo cp -p /etc/secretary/secretary.env /etc/secretary/secretary.env.bak-lucina-node \
  && sudo sed -i 's#^SECRETARY_LUCINA_URL=.*#SECRETARY_LUCINA_URL=$NEW#' /etc/secretary/secretary.env \
  && sudo systemctl restart secretary-server \
  && sleep 3 && curl -fsS -m 15 http://127.0.0.1:8800/v1/health"
echo
say "done. Watch: ssh $NEZ journalctl --user -u lucina-rs -f    Undo: deploy/lucina-rs/rollback.sh"
