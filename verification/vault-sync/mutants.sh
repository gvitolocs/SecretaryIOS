#!/usr/bin/env bash
# Shows that Verus can fail on crates/lucina-vault/src/sync_rules.rs: each line plants one bug in a copy of the shipped
# file, and Verus must refuse it. Run from the repo root (on nezopt, or on nespc: verus-remote . env VERUS=verus bash
# verification/vault-sync/mutants.sh).
set -uo pipefail
verus=${VERUS:-/home/nez/tools/verus/verus-x86-linux/verus}
file=crates/lucina-vault/src/sync_rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/vault-sync-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
total=0
try() {
    local from=$1 to=$2
    total=$((total + 1))
    local text; text=$(<"$file")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf '%s\n' "${text/"$from"/"$to"}" > "$work/mutant.rs"
    local body; body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$work/mutant.rs")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body" > "$work/rules.rs"
    if "$verus" "$work/rules.rs" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        echo "refused: $from -> $to"; refused=$((refused + 1))
    fi
}
# Decide: an edit here overwritten by the server's copy, an edit trashed, a remote edit pushed over, a deletion that wins
# over an edit, a conflict resolved by one side, equal copies pulled again, both absent recorded.
try "        if f.local_is_base {
            return Act::TrashLocal;
        }
        return Act::Push;" "        return Act::TrashLocal;"
try "        if f.remote_is_base {
            return Act::TrashRemote;
        }
        return Act::Pull;" "        return Act::TrashRemote;"
try "    if f.local_is_base {
        return Act::Pull;
    }
    if f.remote_is_base {
        return Act::Push;
    }
    Act::Conflict" "    if f.local_is_base {
        return Act::Pull;
    }
    Act::Push"
try "    if f.local_is_base {
        return Act::Pull;
    }
    if f.remote_is_base {
        return Act::Push;
    }
    Act::Conflict" "    if f.remote_is_base {
        return Act::Push;
    }
    Act::Pull"
try "    if f.local_is_base {
        return Act::Pull;
    }
    if f.remote_is_base {" "    if f.remote_is_base {
        return Act::Pull;
    }
    if f.local_is_base {"
try "    if f.local_is_remote {
        return Act::Record;
    }" "    if f.local_is_remote {
        return Act::Pull;
    }"
try "    if !f.local && !f.remote {
        return Act::Nothing;
    }" "    if !f.local && !f.remote {
        return Act::Record;
    }"
try "    if f.local_is_remote {
        return Act::Record;
    }" ""
# Paths: dot names let through, `.obsidian` folders let through, a backslash or a control byte let through, `//` and a
# trailing `/` let through, a name past the limit let through, any `.obsidian` file pulled, a settings path synced back.
try "            if b == b'.' && name_len == 0 && !(obsidian && i == 0) {" "            if b == b'.' && name_len == 0 && i == 0 && !obsidian {"
try "            if obsidian && i >= 10 {" "            if obsidian && i > 10 && false {"
try "    b < 32 || b == 127 || b == 92
}" "    b < 32 || b == 127
}"
try "            if name_len == 0 {
                // A leading" "            if name_len == 0 && i == 0 {
                // A leading"
try "    if name_len == 0 {
        // A trailing" "    if name_len == 0 && false {
        // A trailing"
try "    if obsidian {
        if is_json(p) {
            return PathClass::PullOnly;
        }
        return PathClass::Skip;
    }" "    if obsidian {
        return PathClass::PullOnly;
    }"
try "            return PathClass::PullOnly;" "            return PathClass::Sync;"
try "    n >= 5 && p[n - 5] == b'.'" "    n >= 5 && p[n - 5] == b'_'"
echo "$refused of $total mutants refused"
exit "$fail"
