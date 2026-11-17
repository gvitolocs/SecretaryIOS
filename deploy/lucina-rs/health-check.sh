#!/bin/sh
# Watchdog for the Lucina user services (installed as ~/.local/bin/lucina-rs-health-check, run by
# lucina-rs-health.timer every 2 minutes).
# - A unit of this script's `units` list is down with nobody having asked for it, in either of two
#   states, and comes back:
#   * inactive with no live maintenance marker: started. Until 2026-11-17 this was the opposite rule
#     ("inactive: stopped on purpose, do nothing"), read off the unit's state alone; earlyoom's
#     SIGTERM left lucina-rs inactive for 23 minutes behind it (incident:lucina-rs-earlyoom:20261117).
#     Stopped on purpose is what `lucina-maint stop <unit> --reason "..."` writes
#     (deploy/lucina-maint), never an inference from a state: no marker, or one past its until=, and
#     the unit comes back. Restart=always brings a killed service back in RestartSec on its own; this
#     is the 2-minute backstop for an inactive unit that outlives that - a unit file on the host that
#     predates it, a bare `systemctl stop`.
#   * failed: reset-failed, then started. This is the state Restart= leaves behind once a unit spends
#     its StartLimitBurst (lucina-agentd at RestartSec=2: about 20 s of crash looping), and systemd
#     queues nothing more after it, so without this branch a crash loop that spends the burst stays
#     down with nothing said. No marker excuses it: a stop on purpose goes through `systemctl stop`
#     and leaves a unit inactive, never failed.
#   A unit that is activating (a deploy mid-restart: ExecStartPost waits on /health) or deactivating
#   is neither state, and is left alone.
# - /health fails twice in a row (it answers 503 only when a background loop died:
#   Telegram poller, reminder scheduler, Discord, Bot Hospital, delivery monitor):
#   restart. One failure is only remembered, so a slow answer during a busy turn
#   never restarts a healthy process.
# - An episode, not a tick: a unit is reported once, by the run that first finds it down.
#   $episode/<unit>.reported marks the episode and goes when the unit is seen active again, so the next
#   time it comes down reports again; the non-zero exit now means "came down", not "is down".
# - Both units start through infisical-broker exec, so while its upstream is down a start cannot work.
#   The run probes that upstream once, only when a broker-wrapped unit ($broker_units) is about to be
#   started, and a failed probe starts none of them: they wait for it, and report once as above.
# A report is the line it prints plus a non-zero exit at the end of the run: that exit is what makes
# OnFailure=pokoin-notify@%n.service on deploy/lucina-rs-health.service notify the owner, once per
# episode rather than once per tick. Both recoveries report, the failed one included: silence is what
# let the 23 minutes happen. A unit still down two minutes later is said to the journal and started
# again, but not to the owner: it is the episode that has already been reported.
units="lucina-rs.service lucina-agentd.service"
# The units whose ExecStart runs through `infisical-broker exec` (on nezopt, both: the host's
# 70-infisical.conf drop-ins). Only these wait for the broker probe; a unit that is not broker-wrapped
# is left out of this list and starts whatever the broker is doing.
broker_units="lucina-rs.service lucina-agentd.service"
# Answered for real by infisical-brokerd on the host; LUCINA_BROKER_UPSTREAM exists for the tests.
broker_upstream="${LUCINA_BROKER_UPSTREAM:-http://127.0.0.1:8088/api/status}"
unit=lucina-rs.service
state="${XDG_RUNTIME_DIR:-/tmp}/lucina-rs-health.failures"
episode="${XDG_RUNTIME_DIR:-/tmp}/lucina-watchdog"
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

# 0 when $1 starts through `infisical-broker exec`, so its start depends on that upstream answering.
broker_wrapped() {
    for b in $broker_units; do
        [ "$b" = "$1" ] && return 0
    done
    return 1
}

# The broker upstream is probed at most once a tick, and only when a start of a broker-wrapped unit is
# on the table: a tick with nothing to start has no business knocking on the secret store. Held in
# $broker_up once known (yes/down), empty while unknown.
broker_up=''
broker_ok() {
    if [ -z "$broker_up" ]; then
        if curl -fsS -m 5 -o /dev/null "$broker_upstream"; then
            broker_up=yes
        else
            broker_up=down
        fi
    fi
    [ "$broker_up" = yes ]
}

# The episode file is written the first time a unit coming down is reported, and removed when the unit
# is seen active: it is what makes the next failure a new episode again. umask 077 so the directory is
# 0700 and the file 0600, the way anything living in $XDG_RUNTIME_DIR next to other programs should.
episode_mark() {
    (umask 077 && mkdir -p "$episode" && : >"$episode/$1.reported")
}

started=''
reported=''
waiting=''
for u in $units; do
    # LoadState first: show answers ActiveState=inactive for a unit this host does not have at all,
    # and starting that is a failure, not a recovery.
    [ "$(systemctl --user show -p LoadState --value "$u")" = loaded ] || continue
    st=$(systemctl --user show -p ActiveState --value "$u")
    if [ "$st" = active ]; then
        rm -f "$episode/$u.reported"
        continue
    fi
    case $st in
        failed) ;;
        inactive) marker_holds "$u" && continue ;;
        *) continue ;;
    esac
    new=''
    [ -f "$episode/$u.reported" ] || new=1
    if broker_wrapped "$u" && ! broker_ok; then
        # A start now would only fail and leave the unit failed again, so the episode waits.
        waiting="$waiting $u"
        if [ -n "$new" ]; then
            episode_mark "$u"
            reported=1
        fi
        continue
    fi
    case $st in
        failed)
            echo "$u: failed, resetting and starting" >&2
            systemctl --user reset-failed "$u"
            ;;
        *)
            echo "$u: inactive with no maintenance marker, starting" >&2
            ;;
    esac
    systemctl --user start "$u"
    # Only the unit /health speaks for owns the counter: a start of it replaces the process the
    # strikes were counting, so a strike from before it cannot be the first of the two.
    [ "$u" = "$unit" ] && rm -f "$state"
    started=$u
    if [ -n "$new" ]; then
        episode_mark "$u"
        reported=1
    fi
done
if [ -n "$waiting" ]; then
    echo "${waiting# }: the Infisical broker upstream is down, not starting, waiting for it" >&2
fi
if [ -n "$started" ] || [ -n "$reported" ]; then
    # Nothing else runs this tick: that process is seconds old, and /health is not a judgement on a
    # unit still coming up. The next run looks at all of it again, two minutes away.
    if [ -n "$reported" ]; then
        # Only a new episode exits non-zero, and that exit is what OnFailure notifies on: an episode
        # already reported is the journal's business, not the owner's.
        exit 1
    fi
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
