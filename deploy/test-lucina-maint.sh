#!/bin/sh
# Tests for deploy/lucina-maint, the command that writes the maintenance marker the watchdog reads.
# Nothing here touches a real unit: a fake systemctl answers from files and records every call, HOME
# and XDG_STATE_HOME are a scratch tree removed on exit including on failure. Run by
# scripts/check-deploy-sh.sh, which is an `lagent merge` gate (.lucina/pipeline.toml, "deploy shell tests").
set -eu

CDPATH=
here=$(cd "$(dirname "$0")" && pwd)
maint=$here/lucina-maint
base=${TMPDIR:-$HOME/scratch/tmp}
mkdir -p "$base"
tmp=$(mktemp -d "$base/lucina-maint-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM HUP

export HOME=$tmp/home
export XDG_STATE_HOME=$HOME/.local/state
export fake_calls=$tmp/fake_calls
export fake_units=$tmp/fake_units
mkdir -p "$XDG_STATE_HOME" "$tmp/bin" "$fake_units"

cat >"$tmp/bin/systemctl" <<'STUB'
#!/bin/sh
# Answers from $fake_units (one file per unit, holding its state). Two units stand in for a command
# that fails: nostop.service cannot be stopped, nosuch.service cannot be started.
printf 'systemctl %s\n' "$*" >>"$fake_calls"
[ "${1:-}" = --user ] && shift
action=${1:-}
unit=${2:-}
[ -f "$fake_units/$unit" ] || exit 4
case $action in
    start)
        case $unit in
            *nosuch*) exit 5 ;;
        esac
        printf 'active\n' >"$fake_units/$unit"
        ;;
    stop)
        case $unit in
            *nostop*) exit 5 ;;
        esac
        printf 'inactive\n' >"$fake_units/$unit"
        ;;
    is-active) [ "$(cat "$fake_units/$unit")" = active ] ;;
    *)
        printf 'fake systemctl: unsupported: %s\n' "$*" >&2
        exit 9
        ;;
esac
STUB
chmod 755 "$tmp/bin/systemctl"
export PATH="$tmp/bin:$PATH"

state=$XDG_STATE_HOME/lucina/maintenance
marker_parent=$XDG_STATE_HOME/lucina
rs=lucina-rs.service
fails=''
: >"$fake_calls"
printf 'active\n' >"$fake_units/$rs"
printf 'active\n' >"$fake_units/nostop.service"
printf 'active\n' >"$fake_units/nosuch.service"

# run <want-exit> <cmd...>: the command under test, its status checked, its output in $out.
run() {
    want=$1
    shift
    rc=0
    out=$("$@" 2>&1) || rc=$?
    if [ "$rc" != "$want" ]; then
        printf '  FAIL: exit %s, wanted %s: %s\n        %s\n' "$rc" "$want" "$*" "$out" >&2
        fails="$fails\n  - wrong exit status: $* (got $rc, wanted $want)"
        return
    fi
}
check() { # check <what> <test-command...>
    what=$1
    shift
    if "$@"; then
        printf '  ok: %s\n' "$what"
    else
        printf '  FAIL: %s\n        %s\n' "$what" "$*" >&2
        fails="$fails\n  - $what"
    fi
}

calls_is() { grep -q "^$1\$" "$fake_calls"; }
says() { printf '%s\n' "$out" | grep -q -- "$1"; }
marker_has() { grep -q "^$2=$3\$" "$state/$1"; }
no_marker() { [ ! -e "$state/$1" ]; }
# The residue a marker write leaves is a `<unit>.new.<pid>` temp file. `..` aimed one of them at the
# directory *above* the markers, where a failed stop cannot clean it (finding 3, JD-R016), so a refusal
# is checked in both directories and not only for the marker itself.
nothing_written() {
    for d in "$state" "$marker_parent"; do
        for f in "$d"/*.new.*; do
            [ -e "$f" ] || continue
            printf 'residue: %s\n' "$f" >&2
            return 1
        done
    done
}
# Nothing hidden either: `.hidden` as a unit name would have become exactly that.
no_hidden_marker() {
    [ -d "$state" ] || return 0
    for f in "$state"/.*; do
        case ${f##*/} in
            . | ..) ;;
            *) return 1 ;;
        esac
    done
}
one_marker() { [ "$(ls "$state" | wc -l | tr -dc 0-9)" = 1 ]; }
# minutes the marker for lucina-rs has left, rounded down, within <low>..<high>
minutes_left() {
    until=$(sed -n 's/^until=//p' "$state/$rs")
    left=$(( (until - $(date +%s)) / 60 ))
    [ "$left" -ge "$1" ] && [ "$left" -le "$2" ]
}
until_is_future() { [ "$(sed -n 's/^until=//p' "$1")" -gt "$(date +%s)" ]; }

printf 'lucina-maint: stop writes the marker, start removes it\n'

run 0 sh "$maint" stop lucina-rs --reason 'rebuild the embedding index' --for 30
check 'the marker is named after the full unit' test -f "$state/$rs"
check '... carries the reason' marker_has "$rs" reason 'rebuild the embedding index'
check '... the who' marker_has "$rs" who "$(id -un)"
check '... the unit' marker_has "$rs" unit "$rs"
check '... and an until= 30 minutes out' minutes_left 29 30
check 'the unit was stopped' calls_is 'systemctl --user stop lucina-rs.service'
check 'one marker, not one per spelling' one_marker

run 0 sh "$maint" ls
check 'ls says what is stopped, why and for how long' \
    says 'lucina-rs.service.*valid, 30 min left.*rebuild the embedding index'

run 0 sh "$maint" start lucina-rs.service
check 'the unit was started' calls_is 'systemctl --user start lucina-rs.service'
check 'the marker is removed' no_marker "$rs"

run 0 sh "$maint" ls
check 'ls says there is nothing once the window is closed' says 'no maintenance markers'

run 0 sh "$maint" stop lucina-rs --reason 'a window that ran out'
sed 's/^until=.*/until=1/' "$state/$rs" >"$state/$rs.old"
mv -f "$state/$rs.old" "$state/$rs"
run 0 sh "$maint" ls
check 'ls calls an expired window expired, as the watchdog does' says 'expired [0-9]* min ago'
run 0 sh "$maint" start lucina-rs
check 'start clears an expired marker too' no_marker "$rs"

run 0 sh "$maint" stop lucina-rs --reason 'a change that needs Lucina down'
check '--for defaults to 120 minutes' minutes_left 119 120
run 0 sh "$maint" start lucina-rs
check 'start finds the marker by the bare name too' no_marker "$rs"

printf 'lucina-maint: a marker only ever describes a stop that happened\n'

run 2 sh "$maint" stop nostop --reason 'try again'
check 'a stop that failed leaves no marker' no_marker nostop.service
check '... and the stop was attempted' calls_is 'systemctl --user stop nostop.service'

run 0 sh "$maint" stop nosuch --reason 'held open'
run 1 sh "$maint" start nosuch
check 'a start that failed keeps the marker' test -f "$state/nosuch.service"

printf 'lucina-maint: it refuses to guess, and says why\n'
for args in 'stop lucina-rs' 'stop lucina-rs --reason' 'stop lucina-rs --reason x --for' \
    'stop lucina-rs --reason x --for 0' 'stop lucina-rs --reason x --for 2h' \
    'stop lucina/../escape --reason x' 'stop lucina-rs lucina-agentd --reason x' \
    'stop lucina-rs --mode 30' 'restart lucina-rs' 'start' 'ls lucina-rs.service' ''; do
    run 2 sh "$maint" $args
    check "  refused without writing: $args" no_marker "$rs"
    check "  the refusal said why: $args" test -n "$out"
done

# The names that passed every check until JD-R016 found them: `.` and `..` hold neither a slash nor a
# space, and both match `*.*`, so they kept their name and became a path inside the state directory.
# A hidden file is the same mistake one step away.
for bad in . .. .hidden; do
    run 2 sh "$maint" stop "$bad" --reason 'pretending to be a unit'
    check "  '$bad' is refused before anything is written" nothing_written
    check "  ... and named as what it is: '$bad'" says "not a unit name"
    run 2 sh "$maint" start "$bad"
    check "  start refuses '$bad' too" nothing_written
done
check 'no hidden marker of any of them is in the maintenance directory' no_hidden_marker

long=$(printf '%0300d' 0)
run 2 sh "$maint" stop "$long" --reason 'a name no unit has'
# Refused for its length, not by a write that happened to fail on NAME_MAX: the message is the proof.
check 'a 300-character name is refused by its length' says 'the cap is 200'
check '... and nothing was written' nothing_written

for weird in 'lucina-rs;rm -rf' 'lucina-rs$(whoami)' 'lucina-rs|tee' 'LUCINA_RS=agentd'; do
    run 2 sh "$maint" stop "$weird" --reason 'shell, not a unit'
    check "  a shell metacharacter is not a unit name: $weird" says 'only letters, digits'
    check "  ... and nothing was written: $weird" nothing_written
done

printf 'lucina-maint: the format the watchdog parses\n'

# One line per key: `until=` read with sed is the whole contract, so a reason carrying a newline must
# not become a key of its own.
run 0 sh "$maint" stop lucina-rs --reason "$(printf 'first\nuntil=1\nsecond')"
check 'a newline in the reason stays inside reason=' marker_has "$rs" reason 'first until=1 second'
check '... and there is exactly one until=' test "$(grep -c '^until=' "$state/$rs")" = 1
check '... the real one, still in the future' until_is_future "$state/$rs"

run 0 sh "$maint" help
check 'help prints the usage and exits 0' says 'usage: lucina-maint stop'

if [ -n "$fails" ]; then
    printf 'lucina-maint: FAILED%b\n' "$fails" >&2
    exit 1
fi
printf 'lucina-maint: every case passed\n'
