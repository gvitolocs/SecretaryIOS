#!/bin/sh
# Tests for the systemd unit files this repository ships, for the keys the incident
# incident:lucina-rs-earlyoom:20261117 decided on: a service that ends cleanly comes back, the start
# limit stays something the watchdog can recover from, a unit ranks below a build in earlyoom's order,
# and the stop that every deploy hit is the one the unit says.
#
# Two layers, because only one of them is a property of the repo:
#  - the keys, read out of the file: always checked, host-independent;
#  - `systemd-analyze --user verify`, which starts and stops nothing but reads the unit the way this
#    host would: it fails on a service whose ExecStart= binary is not installed here, and that is a
#    fact about the host, so it is said rather than counted against the repo. Where there is no
#    systemd-analyze, or no usable --user manager, the keys above are the check and this says so.
# Run by scripts/check-deploy-sh.sh, the `lagent merge` gate "deploy shell tests".
set -eu

CDPATH=
here=$(cd "$(dirname "$0")" && pwd)
fails=''

ok() { printf '  ok: %s\n' "$1"; }
fail() {
    printf '  FAIL: %s\n' "$1" >&2
    fails="$fails\n  - $1"
}
check() { # check <what> <test-command...>
    what=$1
    shift
    if "$@"; then
        ok "$what"
    else
        fail "$what"
    fi
}

# key_in <file> <section> <key>: the value of key in [section], empty when it is not there.
key_in() {
    awk -v want="$2" -v key="$3" '
        { sub(/^[ \t]+/, "") }
        $0 == "" || $0 ~ /^[#;]/ { next }
        /^\[/ { inw = ($0 == "[" want "]"); next }
        inw && index($0, key "=") == 1 { sub(/^[^=]*=/, ""); print; exit }
    ' "$1"
}
# These keys are whole seconds or bare numbers in these files; 10s is read as 10.
num() { printf '%s\n' "$(printf '%s' "$1" | tr -dc 0-9)"; }
has_key() { [ -n "$(key_in "$1" "$2" "$3")" ]; }
key_is() { [ "$(key_in "$1" "$2" "$3")" = "$4" ]; }
key_between() {
    v=$(num "$(key_in "$1" "$2" "$3")")
    [ -n "$v" ] && [ "$v" -ge "$4" ] && [ "$v" -le "$5" ]
}
# A start limit only means something if the burst can actually be spent inside its own interval:
# burst x RestartSec has to fit in it, or the unit never reaches failed and the counter is decoration.
limit_trips() {
    f=$1
    interval=$(num "$(key_in "$f" Unit StartLimitIntervalSec)")
    burst=$(num "$(key_in "$f" Unit StartLimitBurst)")
    sec=$(num "$(key_in "$f" Service RestartSec)")
    [ -n "$interval" ] && [ -n "$burst" ] && [ -n "$sec" ] &&
        [ "$((burst * sec))" -le "$interval" ] && [ "$burst" -le 20 ]
}

for unit in lucina-rs lucina-agentd; do
    f=$here/$unit.service
    printf '%s.service: a clean exit comes back\n' "$unit"
    check 'Restart=always, not on-failure' key_is "$f" Service Restart always
    # At least a couple of seconds so a hard failure cannot spin the CPU, and far below the 2-minute
    # watchdog tick so the unit is back long before anything else gets the chance to notice.
    check 'RestartSec between 2 and 30 s' key_between "$f" Service RestartSec 2 30
    check 'the start limit is named in [Unit]' has_key "$f" Unit StartLimitIntervalSec
    check 'and can trip inside its own interval' limit_trips "$f"
    # earlyoom ranks by oom_score_adj, and an unprivileged user unit cannot go below its manager's
    # 100: this is the floor, and the lowest place a service can hold relative to a build.
    check 'OOMScoreAdjust=100, the floor for a user unit' key_is "$f" Service OOMScoreAdjust 100
    # The stop every deploy used to hit: SIGTERM to the main process, its own deadline, then SIGKILL
    # to the cgroup. Two keys the installed 90-stop.conf drop-in carried, now in the repo.
    check 'TimeoutStopSec between 5 and 30 s' key_between "$f" Service TimeoutStopSec 5 30
    check 'KillMode=mixed' key_is "$f" Service KillMode mixed
    check 'the unit names the systemd rule that keeps a deploy from fighting Restart=' \
        grep -q '^#.*systemd.service(5)' "$f"
done

printf 'lucina-rs-health.service: the watchdog reports what it had to start\n'
h=$here/lucina-rs-health.service
check 'OnFailure= on the repo notify path' key_is "$h" Unit OnFailure 'pokoin-notify@%n.service'
check 'it is a oneshot' key_is "$h" Service Type oneshot
check 'and runs the watchdog these tests cover' \
    key_is "$h" Service ExecStart '%h/.local/bin/lucina-rs-health-check'

# first_line <file> <pattern>: line number of the first match, empty when the file has none.
first_line() { grep -n -- "$2" "$1" | head -1 | cut -d: -f1 || true; }
# comes_first <file> <pattern a> <pattern b>: both are in the file and a is above b.
comes_first() {
    a=$(first_line "$1" "$2")
    b=$(first_line "$1" "$3")
    [ -n "$a" ] && [ -n "$b" ] && [ "$a" -lt "$b" ]
}
no_line() { ! grep -q -- "$2" "$1"; }

# An install script builds and installs, so no test runs one; what is checkable is that the unit file
# is part of it, in the order systemd needs. Until JD-R016 finding 2 that was true of lucina-rs only,
# and the keys above reached nezopt for one of the two services.
printf 'the deploy that carries these keys to the host\n'
for unit in lucina-rs lucina-agentd; do
    i=$here/$unit/install.sh
    check "$unit/install.sh installs $unit.service" comes_first "$i" \
        "install -m 0644 \"\$REPO/deploy/$unit.service\"" 'systemctl --user daemon-reload'
    check "... reloads before it restarts" comes_first "$i" \
        'systemctl --user daemon-reload' "systemctl --user restart $unit.service"
    # The host's drop-ins hold what the unit file must not carry (the infisical-broker ExecStart, the
    # Honcho MCP line, secret references): installing a unit file is never a reason to touch them.
    check "... and leaves $unit.service.d alone" no_line "$i" 'rm.*service\.d'
done
check 'the agentd unit says its file is installed by that script, not by hand cp' \
    grep -q '^#.*deploy/lucina-agentd/install.sh' "$here/lucina-agentd.service"
check '... and no longer tells anyone to copy it in' no_line "$here/lucina-agentd.service" \
    '^# *cp deploy/lucina-agentd.service'

printf 'systemd-analyze --user verify (static: nothing is started or stopped)\n'
if ! command -v systemd-analyze >/dev/null 2>&1; then
    printf '  no systemd-analyze on this host: the keys above are the check\n'
else
    for f in lucina-rs.service lucina-agentd.service lucina-rs-health.service lucina-rs-health.timer; do
        out=$(systemd-analyze --user verify "$here/$f" 2>&1) && rc=0 || rc=$?
        if [ "$rc" = 0 ] && [ -z "$out" ]; then
            ok "$f verifies clean"
            continue
        fi
        case $out in
            *'Failed to create directory'* | *'Failed to connect'*)
                printf '  %s: no usable --user manager here (%s): the keys above are the check\n' \
                    "$f" "$(printf '%s\n' "$out" | head -1)"
                continue
                ;;
        esac
        # A binary this host has not installed is a host fact, not a defect in the unit file.
        rest=$(printf '%s\n' "$out" | grep -v 'is not executable' || true)
        if [ "$rc" = 0 ] && [ -z "$rest" ]; then
            printf '  %s: verifies, apart from a binary this host has not installed\n' "$f"
            continue
        fi
        fail "$f: $out"
    done
fi

if [ -n "$fails" ]; then
    printf 'units: FAILED%b\n' "$fails" >&2
    exit 1
fi
printf 'units: every case passed\n'
