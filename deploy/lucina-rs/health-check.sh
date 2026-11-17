#!/bin/sh
# Watchdog for the Lucina user services (installed as ~/.local/bin/lucina-rs-health-check, run by
# lucina-rs-health.timer every 2 minutes).
# - A unit is inactive and has no live maintenance marker: start it and report it once. Until
#   2026-11-17 this was the opposite rule ("inactive: stopped on purpose, do nothing"), read off the
#   unit's state alone; earlyoom's SIGTERM left lucina-rs inactive for 23 minutes behind it
#   (incident:lucina-rs-earlyoom:20261117). Stopped on purpose is what `lucina-maint stop <unit>
#   --reason "..."` writes (deploy/lucina-maint), never an inference from a state: no marker, or one
#   past its until=, and the unit comes back. Restart=always brings a killed service back in
#   RestartSec on its own; this is the 2-minute backstop for an inactive unit that outlives that -
#   a unit file on the host that predates it, a start limit that tripped, a bare `systemctl stop`.
#   A unit that is activating (a deploy mid-restart: ExecStartPost waits on /health) is not
#   inactive, and failed units have their own branch below.
# - The unit is failed (crash loop hit StartLimitBurst): reset it and start it again.
# - /health fails twice in a row (it answers 503 only when a background loop died:
#   Telegram poller, reminder scheduler, Discord, Bot Hospital, delivery monitor):
#   restart. One failure is only remembered, so a slow answer during a busy turn
#   never restarts a healthy process.
# A report is the line it prints plus a non-zero exit at the end of the run: that exit is what makes
# OnFailure=pokoin-notify@%n.service on deploy/lucina-rs-health.service notify the owner, once, since
# the next run finds the unit active again.
units="lucina-rs.service lucina-agentd.service"
unit=lucina-rs.service
state="${XDG_RUNTIME_DIR:-/tmp}/lucina-rs-health.failures"
maint="${XDG_STATE_HOME:-$HOME/.local/state}/lucina/maintenance"

# 0 when $1 was stopped on purpose and that has not expired yet. A marker with no readable until=
# excuses nothing: an expired one and a broken one both mean start it, which is the safe way round.
marker_holds() {
    [ -f "$maint/$1" ] || return 1
    until=$(sed -n 's/^until=//p' "$maint/$1" | tail -1)
    case $until in
        '' | *[!0-9]*) return 1 ;;
    esac
    [ "$(date +%s)" -lt "$until" ]
}

started=''
for u in $units; do
    # LoadState first: show answers ActiveState=inactive for a unit this host does not have at all,
    # and starting that is a failure, not a recovery.
    [ "$(systemctl --user show -p LoadState --value "$u")" = loaded ] || continue
    [ "$(systemctl --user show -p ActiveState --value "$u")" = inactive ] || continue
    marker_holds "$u" && continue
    echo "$u: inactive with no maintenance marker, starting" >&2
    systemctl --user start "$u"
    started=$u
done
if [ -n "$started" ]; then
    # Nothing else runs this tick: that process is seconds old, and /health is not a judgement on a
    # unit still coming up. The next run looks at all of it again, two minutes away.
    exit 1
fi
if systemctl --user is-failed --quiet "$unit"; then
    echo "lucina-rs: unit failed, resetting and starting" >&2
    systemctl --user reset-failed "$unit"
    systemctl --user start "$unit"
    rm -f "$state"
    exit 0
fi
systemctl --user is-active --quiet "$unit" || exit 0
if curl -fsS -m 10 -o /dev/null http://127.0.0.1:8790/health; then
    rm -f "$state"
    exit 0
fi
n=$(( $(cat "$state" 2>/dev/null || echo 0) + 1 ))
if [ "$n" -ge 2 ]; then
    echo "lucina-rs: /health failed twice in a row, restarting" >&2
    rm -f "$state"
    systemctl --user restart "$unit"
else
    echo "$n" > "$state"
    echo "lucina-rs: /health failed once, checking again next run" >&2
fi
