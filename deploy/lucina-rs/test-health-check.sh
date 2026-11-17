#!/bin/sh
# Tests for the watchdog deploy/lucina-rs/health-check.sh (installed as lucina-rs-health-check, run by
# lucina-rs-health.timer). Nothing here touches a real unit: a fake systemctl answers from files and
# records every call, curl is a stub, and HOME/XDG_STATE_HOME/XDG_RUNTIME_DIR are a scratch tree that
# is removed on exit including on failure. Run by scripts/check-deploy-sh.sh, which is an
# `lagent merge` gate (.lucina/pipeline.toml, "deploy shell tests").
#
# Markers are written by the real deploy/lucina-maint, never by hand: the two files agree on the
# format because this test reads what that command wrote.
set -eu

CDPATH=
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
watch=$here/health-check.sh
maint=$repo/deploy/lucina-maint
base=${TMPDIR:-$HOME/scratch/tmp}
mkdir -p "$base"
tmp=$(mktemp -d "$base/lucina-watchdog-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM HUP

export HOME=$tmp/home
export XDG_STATE_HOME=$HOME/.local/state
export XDG_RUNTIME_DIR=$tmp/run
export fake_calls=$tmp/fake_calls
export fake_units=$tmp/fake_units
mkdir -p "$XDG_STATE_HOME" "$XDG_RUNTIME_DIR" "$tmp/bin" "$fake_units"

cat >"$tmp/bin/systemctl" <<'STUB'
#!/bin/sh
# Answers from $fake_units: one file per unit, holding its ActiveState; no file means the unit is not
# installed on this host. start/restart/stop move the state, so a test may run the watchdog twice.
printf 'systemctl %s\n' "$*" >>"$fake_calls"
[ "${1:-}" = --user ] && shift
action=${1:-}
shift
unit=''
prop=''
need=''
for a in "$@"; do
    if [ -n "$need" ]; then
        prop=$a
        need=''
        continue
    fi
    case $a in
        -p | --property) need=1 ;;
        -p=* | --property=*) prop=${a#*=} ;;
        -*) ;;
        *) unit=$a ;;
    esac
done
state_of() {
    # A unit this host does not have is `inactive` to `show -p ActiveState` and only LoadState says
    # `not-found` - which is the whole reason the watchdog checks LoadState first, so the stub answers
    # this way rather than inventing a fourth state (JD-R016 finding 4a).
    if [ -f "$fake_units/$1" ]; then cat "$fake_units/$1"; else printf 'inactive\n'; fi
}
case $action in
    show)
        case $prop in
            LoadState)
                if [ -f "$fake_units/$unit" ]; then printf 'loaded\n'; else printf 'not-found\n'; fi
                ;;
            ActiveState) state_of "$unit" ;;
            *) printf '%s=%s\n' "${prop:-ActiveState}" "$(state_of "$unit")" ;;
        esac
        ;;
    is-active) [ "$(state_of "$unit")" = active ] ;;
    is-failed) [ "$(state_of "$unit")" = failed ] ;;
    start | restart) printf 'active\n' >"$fake_units/$unit" ;;
    stop) printf 'inactive\n' >"$fake_units/$unit" ;;
    reset-failed) [ -f "$fake_units/$unit" ] ;;
    *)
        printf 'fake systemctl: unsupported: %s\n' "$*" >&2
        exit 9
        ;;
esac
STUB
cat >"$tmp/bin/curl" <<'STUB'
#!/bin/sh
# curl answers per URL, because a tick now makes two unrelated probes and a test has to hold one up
# while it fails the other: HEALTH_EXIT for the /health check, BROKER_EXIT for the broker upstream,
# both 0 (answering) when unset. The broker URL is a .invalid host, so even a stub that stops
# matching can never knock on the real infisical-brokerd.
printf 'curl %s\n' "$*" >>"$fake_calls"
for a in "$@"; do
    case $a in
        http://* | https://*)
            case $a in
                "$fake_broker_url") exit "${BROKER_EXIT:-0}" ;;
                */health) exit "${HEALTH_EXIT:-0}" ;;
                *)
                    printf 'fake curl: unsupported URL: %s\n' "$a" >&2
                    exit 9
                    ;;
            esac
            ;;
    esac
done
exit 0
STUB
chmod 755 "$tmp/bin/systemctl" "$tmp/bin/curl"
export PATH="$tmp/bin:$PATH"

maint_dir=$XDG_STATE_HOME/lucina/maintenance
counter=$XDG_RUNTIME_DIR/lucina-rs-health.failures
episode_dir=$XDG_RUNTIME_DIR/lucina-watchdog
rs=lucina-rs.service
agentd=lucina-agentd.service
fails=''

# The watchdog asks this variable for the broker upstream; production uses the default (the real
# infisical-brokerd on 127.0.0.1:8088), here it is a host that cannot resolve.
fake_broker_url=http://broker.invalid/api/status
export fake_broker_url
export LUCINA_BROKER_UPSTREAM=$fake_broker_url

# reset <rs-state> <agentd-state>: fresh call log, no markers, no /health counter, no episode file.
# `-` leaves that unit uninstalled on this host.
reset() {
    : >"$fake_calls"
    rm -rf "$maint_dir" "$episode_dir"
    rm -f "$fake_units/$rs" "$fake_units/$agentd" "$counter"
    unset HEALTH_EXIT BROKER_EXIT
    [ "$1" = - ] || printf '%s\n' "$1" >"$fake_units/$rs"
    [ "$2" = - ] || printf '%s\n' "$2" >"$fake_units/$agentd"
}

# run_watch leaves the watchdog's exit status in rc and its report lines in log.
run_watch() {
    rc=0
    log=$(sh "$watch" 2>&1 >/dev/null) || rc=$?
}

check() { # check <what> <test-command...>
    what=$1
    shift
    if "$@"; then
        printf '  ok: %s\n' "$what"
    else
        printf '  FAIL: %s\n' "$what" >&2
        printf '        calls: %s\n        rc: %s\n        log: %s\n' \
            "$(tr '\n' ';' <"$fake_calls")" "$rc" "$log" >&2
        fails="$fails\n  - $what"
    fi
}

calls_is() { grep -q "^$1\$" "$fake_calls"; }
call_count() {
    n=$(grep -c "^$1\$" "$fake_calls" || true)
    [ "$n" = "$2" ]
}
# called_before <call a> <call b>: both were made this tick and a came first - the order the recovery
# needs, since a start before the reset-failed would be refused by the very limit being reset.
called_before() {
    a=$(grep -n "^$1\$" "$fake_calls" | head -1 | cut -d: -f1 || true)
    b=$(grep -n "^$2\$" "$fake_calls" | head -1 | cut -d: -f1 || true)
    [ -n "$a" ] && [ -n "$b" ] && [ "$a" -lt "$b" ]
}
started() { calls_is "systemctl --user start $1"; }
not_started() { ! calls_is "systemctl --user start $1"; }
not_reset_failed() { ! calls_is "systemctl --user reset-failed $1"; }
restarted() { calls_is "systemctl --user restart $1"; }
not_restarted() { ! calls_is "systemctl --user restart $1"; }
reported_inactive() { printf '%s\n' "$log" | grep -q "^$1: inactive with no maintenance marker, starting\$"; }
reported_failed() { printf '%s\n' "$log" | grep -q "^$1: failed, resetting and starting\$"; }
# A tick that recovers nothing says nothing at all: no start, no reset, no /health judgement.
not_reported() { [ -z "$log" ]; }
exited() { [ "$rc" = "$1" ]; }
exited_nonzero() { [ "$rc" != 0 ]; }
counter_holds() { [ -f "$counter" ] && [ "$(cat "$counter")" = "$1" ]; }
no_counter() { [ ! -e "$counter" ]; }
started_once() {
    call_count "systemctl --user start $rs" 1
}
# The episode file is the watchdog's own; a case that needs one to exist beforehand makes it first.
mark_episode() {
    mkdir -p "$episode_dir" && : >"$episode_dir/$1.reported"
}
episode_marked() { [ -f "$episode_dir/$1.reported" ]; }
no_episode() { [ ! -e "$episode_dir/$1.reported" ]; }
episode_dir_mode() { [ "$(stat -c '%a' "$episode_dir")" = 700 ]; }
broker_probes() { call_count "curl -fsS -m 5 -o /dev/null $fake_broker_url" "$1"; }
nothing_started() { ! grep -q '^systemctl --user start ' "$fake_calls"; }
all_marked() {
    for u in "$@"; do
        episode_marked "$u" || return 1
    done
}
# next_tick [rs-state] [agentd-state]: the run two minutes later. The call log is cleared first, so a
# check about this tick cannot pass on the calls the tick before it made.
next_tick() {
    : >"$fake_calls"
    if [ $# -ge 2 ]; then
        [ "$1" = - ] || printf '%s\n' "$1" >"$fake_units/$rs"
        [ "$2" = - ] || printf '%s\n' "$2" >"$fake_units/$agentd"
    fi
}
# waits_for <unit...>: the tick said, on one line, that every unit named waits for the broker.
waits_for() {
    [ "$(printf '%s\n' "$log" | grep -c 'broker upstream is down' || true)" = 1 ] || return 1
    line=$(printf '%s\n' "$log" | grep 'broker upstream is down')
    for u in "$@"; do
        case " $line " in
            *" $u "* | *" $u:"*) ;;
            *) return 1 ;;
        esac
    done
}

printf 'health-check: the inactive rule (incident:lucina-rs-earlyoom:20261117)\n'

# 1. inactive and no marker: started, and reported.
reset inactive active
run_watch
check 'inactive with no marker starts lucina-rs' started "$rs"
check '... once, not twice' started_once
check '... and reports it by name' reported_inactive "$rs"
check '... and exits non-zero, which is what notifies' exited_nonzero

# 2. inactive with a valid marker: stopped on purpose, so nothing happens.
reset inactive active
sh "$maint" stop "$rs" --reason 'rebuild the embedding index' --for 30 >/dev/null
run_watch
check 'a valid marker means no start' not_started "$rs"
check '... and no report' not_reported
check '... and a clean exit' exited 0

# 3. inactive with an expired marker: the window is over. (Expired by moving until= into the past:
# the alternative is sleeping for --for minutes.)
reset inactive active
sh "$maint" stop "$rs" --reason 'a 30 minute window' --for 30 >/dev/null
sed 's/^until=.*/until=1/' "$maint_dir/$rs" >"$maint_dir/$rs.old"
mv -f "$maint_dir/$rs.old" "$maint_dir/$rs"
run_watch
check 'an expired marker still starts' started "$rs"
check '... and reports' reported_inactive "$rs"
check '... and exits non-zero' exited_nonzero

# 4. a marker with no readable until= is not an excuse.
reset inactive active
mkdir -p "$maint_dir"
printf 'unit=%s\nreason=hand-edited\n' "$rs" >"$maint_dir/$rs"
run_watch
check 'a marker with no until= still starts' started "$rs"

# 5. failed: reset-failed, then start, and reported. A failed unit is the state Restart= leaves behind
# once the start limit is spent, so systemd is not going to bring it back by itself: this is the one
# branch that must not stay silent.
reset failed active
run_watch
check 'a failed lucina-rs is reset' calls_is "systemctl --user reset-failed $rs"
check '... and started, once' started_once
check '... reset first, then started' called_before \
    "systemctl --user reset-failed $rs" "systemctl --user start $rs"
check '... and reported by name' reported_failed "$rs"
check '... and the report exits non-zero, which is what notifies' exited_nonzero

# 5b. the same for lucina-agentd, which is the unit that spends its burst fastest (10 x RestartSec=2,
# about 20 s) and whose [Unit] comment promises this recovery (finding 1, JD-R016). Without it agentd
# stays failed tick after tick, and the owner hears nothing because the watchdog exits 0.
reset active failed
run_watch
check 'a failed lucina-agentd is reset' calls_is "systemctl --user reset-failed $agentd"
check '... and then started' started "$agentd"
check '... reset first, then started' called_before \
    "systemctl --user reset-failed $agentd" "systemctl --user start $agentd"
check '... reported by name' reported_failed "$agentd"
check '... and lucina-rs is left alone' not_started "$rs"
check '... and the run exits non-zero' exited_nonzero

# 5c. failed is never on purpose: lucina-maint stops through `systemctl stop`, which leaves a unit
# inactive, so a marker is not read here.
reset active failed
sh "$maint" stop "$agentd" --reason 'stopped, then it failed on a start' >/dev/null
printf 'failed\n' >"$fake_units/$agentd"
run_watch
check 'a marker does not excuse a failed unit' calls_is "systemctl --user reset-failed $agentd"
check '... and it is started anyway' started "$agentd"

# 6. activating is not inactive: a deploy mid-restart waits on /health in ExecStartPost.
reset activating active
run_watch
check 'an activating unit is not started again' not_started "$rs"
check '... and not reported' not_reported

# 7. lucina-agentd is covered by the same rule.
reset active inactive
run_watch
check 'an inactive lucina-agentd is started' started "$agentd"
check '... and reported by name' reported_inactive "$agentd"
check '... and lucina-rs is not touched' not_started "$rs"
reset active inactive
sh "$maint" stop "$agentd" --reason 'walking its drop-ins' >/dev/null
run_watch
check 'a marked lucina-agentd is left alone' not_started "$agentd"

# 8. a unit this host does not have is not something to start. The two checks on the stub come first:
# they pin the premise the LoadState guard is built on, so a stub that stopped answering like real
# systemctl fails here instead of quietly making the guard untested again.
reset active -
check 'the stub answers LoadState=not-found for a unit that is not here' \
    test "$(systemctl --user show -p LoadState --value "$agentd")" = not-found
check '... and ActiveState=inactive, the answer that makes LoadState the only usable guard' \
    test "$(systemctl --user show -p ActiveState --value "$agentd")" = inactive
: >"$fake_calls"
run_watch
check 'an uninstalled unit is not started' not_started "$agentd"
check '... and nothing is reported' not_reported

printf 'health-check: the rules that were already there\n'

# 9. /health failing once is only remembered.
reset active active
HEALTH_EXIT=22
export HEALTH_EXIT
run_watch
check 'one /health failure does not restart' not_restarted "$rs"
check '... and counts' counter_holds 1
check '... and stays quiet' exited 0

# 10. twice in a row: restart, counter gone.
run_watch
check 'two /health failures restart lucina-rs' restarted "$rs"
check '... and clear the counter' no_counter
# A healthy run clears a strike that is still on the books. The counter has to be put there first:
# `reset` deletes it, so the version of this case that only called reset passed whether or not the
# success path removes the file (JD-R016 finding 4b).
reset active active
printf '1\n' >"$counter"
run_watch
check 'a healthy run clears a strike the last tick left' no_counter
check 'a healthy unit starts nothing' not_started "$rs"
check '... and reports nothing' not_reported

printf 'health-check: an episode, and the Infisical broker upstream\n'

# 11. The 2026-11-07 shape: Infisical answered 502 for hours. Both units start through
# `infisical-broker exec`, so every start failed and left the unit failed again, and the watchdog
# reset-failed, started and notified on every 2-minute tick of it. With the broker down the unit is
# not touched at all and the episode is told once.
reset failed active
BROKER_EXIT=22
export BROKER_EXIT
run_watch
check 'a failed unit with the broker down is not reset-failed' not_reset_failed "$rs"
check '... and not started' not_started "$rs"
check '... but the broker upstream was probed, once' broker_probes 1
check '... and the tick says the unit waits for it' waits_for "$rs"
check '... and the new episode is reported once, by exiting non-zero' exited_nonzero
check '... and the episode is marked' episode_marked "$rs"
check '... in a directory only the owner can read' episode_dir_mode
next_tick
run_watch
check 'the next tick still probes once' broker_probes 1
check '... and still starts nothing' not_started "$rs"
check '... and says so to the journal again' waits_for "$rs"
check '... but does not notify: the episode is already told' exited 0
next_tick
run_watch
check '... nor on the third tick' exited 0
check '... with the episode still marked, so nothing re-notifies' episode_marked "$rs"

# 12. The broker comes back: the recovery runs, and this tick is quiet because the episode has
# already been told. Seeing the unit active again is what closes it.
reset failed active
mark_episode "$rs"
run_watch
check 'a failed unit with the broker up is reset and started' started "$rs"
check '... reset before it is started' called_before \
    "systemctl --user reset-failed $rs" "systemctl --user start $rs"
check '... and the broker was probed first, since a start is on the table' broker_probes 1
check '... and it is still said to the journal' reported_failed "$rs"
check '... but an episode already told is not notified twice' exited 0
next_tick
run_watch
check 'the tick that finds the unit active closes the episode' no_episode "$rs"
check '... and starts nothing' not_started "$rs"

# 13. A second episode reports again: the file is gone, so the next failure is news.
next_tick failed active
run_watch
check 'a unit that comes down again after recovery is a new episode' exited_nonzero
check '... and is recovered' started "$rs"
check '... and marked again' episode_marked "$rs"

# 14. Both units down with the broker down: one tick, one notification, one probe, two files.
reset failed failed
BROKER_EXIT=22
export BROKER_EXIT
run_watch
check 'one outage tick with both units down reports once' exited_nonzero
check '... and probes the broker once for the two of them' broker_probes 1
check '... and starts neither' nothing_started
check '... and marks both episodes' all_marked "$rs" "$agentd"
check '... naming both on the one waiting line' waits_for "$rs" "$agentd"

# 15. Broker up and the start does not stick (earlyoom keeps landing on it): the first tick tells the
# owner, the ticks after it keep trying and keep quiet. The rule of 11, seen without the outage.
reset inactive active
run_watch
check 'a new inactive episode with the broker up reports' exited_nonzero
check '... and starts' started "$rs"
check '... and marks the episode' episode_marked "$rs"
next_tick inactive active
run_watch
check '... and the same episode two minutes later does not' exited 0
check '... though it starts again' started "$rs"
check '... and still says so to the journal' reported_inactive "$rs"
check '... and probes the broker for it, once' broker_probes 1

# 16. A unit stopped on purpose is not an episode, outage or not: there is nothing to start, so the
# broker is not even asked.
reset inactive active
sh "$maint" stop "$rs" --reason 'riding out the Infisical outage' >/dev/null
BROKER_EXIT=22
export BROKER_EXIT
run_watch
check 'a valid marker with the broker down starts nothing' not_started "$rs"
check '... and reports nothing' exited 0
check '... and leaves no episode file' no_episode "$rs"
check '... so the broker is not probed at all' broker_probes 0
check '... and says nothing' not_reported

if [ -n "$fails" ]; then
    printf 'health-check: FAILED%b\n' "$fails" >&2
    exit 1
fi
printf 'health-check: every case passed\n'
