#!/usr/bin/env bash
# Install lucina-agentd and lagent on nezopt from this checkout (user unit, no root):
#   deploy/lucina-agentd/install.sh        (what `lagent deploy lucina-agentd` runs, after deploy-guard check)
# - builds both binaries (cargo is incremental; an old binary must never be installed by mistake);
# - keeps the running ones as ~/.local/bin/<name>.bak-<UTC stamp>-pre-<sha8>;
# - installs deploy/lucina-agentd.service and reloads, so the keys incident:lucina-rs-earlyoom:20261117
#   decided on reach the host instead of staying in git: without this the unit kept the Restart= and
#   OOMScoreAdjust it was hand-copied with, however many times this script ran (JD-R016 finding 2).
#   The drop-ins in lucina-agentd.service.d are the host's and stay as they are (Honcho MCP, the
#   infisical-broker ExecStart, the journal name); they keep overriding whatever they set, the way a
#   drop-in does.
# - restarts lucina-agentd and waits for it to answer `lagent providers`.
# A pipeline job that runs this keeps going while agentd restarts: jobs are their own systemd units.
set -euo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$REPO/target}
BIN=$HOME/.local/bin
UNITS=$HOME/.config/systemd/user
SHA=$(git -C "$REPO" rev-parse --short=8 HEAD)
STAMP=$(date -u +%Y%m%dT%H%M%SZ)

(cd "$REPO" && cargo build --release -p lucina-agentd --bins)

for name in lucina-agentd lagent; do
  [ -f "$BIN/$name" ] && cp -p "$BIN/$name" "$BIN/$name.bak-$STAMP-pre-$SHA"
  install -m 0755 "$TARGET/release/$name" "$BIN/$name.new"
  mv -f "$BIN/$name.new" "$BIN/$name"
done
echo "installed lucina-agentd + lagent at $SHA (previous: *.bak-$STAMP-pre-$SHA)"

mkdir -p "$UNITS"
install -m 0644 "$REPO/deploy/lucina-agentd.service" "$UNITS/"
systemctl --user daemon-reload

# `restart` does not fight the unit's Restart=always: systemd queues no start for a stop it was asked
# for, and a restart job is that suppressed stop plus one start. No maintenance marker wanted: the
# watchdog only reads a marker while the unit is inactive, and a restart leaves it active.
systemctl --user restart lucina-agentd.service
# A deploy overrules an earlier stop on purpose (deploy/lucina-maint wrote that marker, and the unit
# is up now); leaving it would excuse the next unexpected exit until it expired. Cleared through the
# command itself, the way lucina-rs/install.sh does it, because that is what resolves the marker path
# from XDG_STATE_HOME: the hand-worked `rm -f` this replaces always looked under $HOME, so on a host
# that sets that variable it removed a file the watchdog never reads and left the real excuse in place
# (JD-R016 finding 5). This script installs its own copy of lucina-maint, so an agentd deploy never
# depends on the other service having been installed; it is the same file from the same checkout.
install -m 0755 "$REPO/deploy/lucina-maint" "$BIN/lucina-maint"
"$BIN/lucina-maint" start lucina-agentd >/dev/null
for _ in $(seq 1 30); do
  if "$BIN/lagent" --json providers >/dev/null 2>&1; then
    systemctl --user --no-pager --lines=0 status lucina-agentd.service | head -3
    exit 0
  fi
  sleep 1
done
echo "lucina-agentd did not answer within 30 s; roll back: cp -p $BIN/lucina-agentd.bak-$STAMP-pre-$SHA $BIN/lucina-agentd && systemctl --user restart lucina-agentd" >&2
exit 1
