#!/usr/bin/env bash
# ONE COMMAND: back to the Node Lucina for the Secretary API (undoes cutover.sh).
#   1. p1: SECRETARY_LUCINA_URL -> http://100.107.164.33:8788, restart secretary-server.
#   2. nezopt: lucina-rs back to shadow.env (its own timeline file, Honcho read only,
#      no jobs). Lines Rust wrote to the live timeline, its job records and Honcho
#      messages stay: Node reads the same formats. Timeline ownership can stay with nez
#      (Node runs as root).
set -euo pipefail
NEZ=${NEZOPT_HOST:-}
if [ -z "$NEZ" ]; then NEZ=nezopt; nc -z -G 2 nezopt.fritz.box 22 >/dev/null 2>&1 || NEZ=nezopt-remote; fi
P1=${P1_HOST:-p1}
OLD=http://100.107.164.33:8788
say() { printf '[rollback] %s\n' "$*"; }

say "p1: secretary-server -> $OLD"
ssh "$P1" "sudo sed -i 's#^SECRETARY_LUCINA_URL=.*#SECRETARY_LUCINA_URL=$OLD#' /etc/secretary/secretary.env \
  && sudo systemctl restart secretary-server \
  && sleep 3 && curl -fsS -m 15 http://127.0.0.1:8800/v1/health"
echo
say "nezopt: lucina-rs back to shadow"
ssh "$NEZ" 'install -m 0600 /home/nez/Projects/SecretaryIOS-lucina/deploy/lucina-rs/shadow.env ~/.config/lucina-rs/mode.env \
  && systemctl --user restart lucina-rs.service \
  && curl -fsS -m 5 http://127.0.0.1:8790/health | grep -q "\"shadow\":true" && echo "lucina-rs back in shadow mode"'
say "done: the Node Lucina serves the Secretary API again"
