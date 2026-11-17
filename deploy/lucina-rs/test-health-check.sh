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
printf 'curl %s\n' "$*" >>"$fake_calls"
exit "${CURL_EXIT:-0}"
STUB
chmod 755 "$tmp/bin/systemctl" "$tmp/bin/curl"
export PATH="$tmp/bin:$PATH"

maint_dir=$XDG_STATE_HOME/lucina/maintenance
counter=$XDG_RUNTIME_DIR/lucina-rs-health.failures
rs=lucina-rs.service
agentd=lucina-agentd.service
fails=''

# reset <rs-state> <agentd-state>: fresh call log, no markers, no /health counter. `-` leaves that unit
# uninstalled on this host.
reset() {
    : >"$fake_calls"
    rm -rf "$maint_dir"
    rm -f "$fake_units/$rs" "$fake_units/$agentd" "$counter"
    unset CURL_EXIT
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
probe_once() {
    call_count "systemctl --user start $rs" 1
}

printf 'health-check: the inactive rule (incident:lucina-rs-earlyoom:20261117)\n'

# 1. inactive and no marker: started, and reported.
reset inactive active
run_watch
check 'inactive with no marker starts lucina-rs' started "$rs"
check '... once, not twice' probe_once
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
check '... and started, once' probe_once
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
CURL_EXIT=22
export CURL_EXIT
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

if [ -n "$fails" ]; then
    printf 'health-check: FAILED%b\n' "$fails" >&2
    exit 1
fi
printf 'health-check: every case passed\n'
