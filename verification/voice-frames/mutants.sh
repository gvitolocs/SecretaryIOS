#!/usr/bin/env bash
# Shows that Verus can fail on crates/secretary-server/src/voice_rules.rs: each `try` plants one bug in a copy of the
# shipped file, and Verus must refuse it. Run from the repo root, through verify-gate on nezopt:
#   ~/.local/bin/verify-gate -- bash verification/voice-frames/mutants.sh
set -uo pipefail
verus=${VERUS:-/home/nez/tools/verus/verus-x86-linux/verus}
file=crates/secretary-server/src/voice_rules.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/voice-frames-mutants.XXXXXX")
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
# The step and the predictor: a clamp that lets one more through (index high and low, sample high, both widths), a table delta
# too big or too small, a magnitude that is not three bits, the sample or the index left unclamped, the index not clamped
# before the table is read.
try "    } else if i > 88 {
        88" "    } else if i > 89 {
        88"
try "    if i < 0 {
        0" "    if i < -1 {
        0"
try "    } else if v > 32767 {
        32767
    } else {
        v
    }" "    } else if v > 32768 {
        32767
    } else {
        v
    }"
try "    } else if v > 32767 {
        32767
    } else {
        v as i32" "    } else if v > 32768 {
        32767
    } else {
        v as i32"
try "        7 => 8," "        7 => 9,"
try "        _ => -1," "        _ => -2,"
try "let magnitude = (code % 8) as i64;" "let magnitude = (code % 128) as i64;"
try "(clamp_sample64(next), clamp_index(index + index_delta(code)))" "(next as i32, clamp_index(index + index_delta(code)))"
try "(clamp_sample64(next), clamp_index(index + index_delta(code)))" "(clamp_sample64(next), index + index_delta(code))"
try "    let index = clamp_index(index);
    let step" "    let step"
# The block maths: samples per block off by one, a block of 3 bytes taken as a block, the rest of a stream counted wrong,
# a block decoded from one byte too early, one byte too few decoded.
try "        2 * (block - 4) + 1
    }" "        2 * (block - 4)
    }"
try "    if block < 4 {
        0
    } else {
        2 * (block - 4) + 1" "    if block < 3 {
        0
    } else {
        2 * (block - 4) + 1"
try "whole * samples_per_block(block) + partial_samples(rest)" "whole * samples_per_block(block) + partial_samples(rest + 1)"
try "    let mut i = start + 4;" "    let mut i = start + 3;"
try "    if end - start < 4 {
        return;" "    if end - start < 3 {
        return;"
try "        out.push(pred as i16);
        i += 1;" "        i += 1;"
# The assembler: a buffer allowed to reach a whole block, a block decoded from a frame that is too short, a full buffer not
# emptied, a partial block of 4 bytes dropped, the tail kept after the end, a block size past the limit.
try "if self.buf.len() >= self.block {" "if self.buf.len() > self.block {"
try "data.len() - i >= self.block {" "data.len() - i + 1 >= self.block {"
try "out);
                    self.buf.clear();" "out);"
try "        if self.buf.len() >= 4 {" "        if self.buf.len() >= 5 {"
try "        self.buf.clear();
    }
}

/// Linear" "    }
}

/// Linear"
try "            MAX_BLOCK
        } else {" "            MAX_BLOCK + 1
        } else {"
# The resampler: a read past the end of the input (the loop one sample too long, the right neighbour one too far), the
# fractional part not a fraction, the carry over wrong (shift, start, step, last), so that the length law fails.
try "while self.pos <= m * 16000" "while self.pos <= m * 16000 + 16000"
try "input[start + idx as usize] as i32" "input[start + idx as usize + 1] as i32"
try "let frac = self.pos % 16000;" "let frac = self.pos % 16001;"
try "self.pos -= m * 16000;" "self.pos -= m * 15999;"
try "            start = 1;" "            start = 0;"
try "self.pos += step;" "self.pos += step + 1;"
try "self.last = input[input.len() - 1] as i32;" "self.last = input[input.len() - 2] as i32;"
# The cap: a chunk allowed past the room, the cap reached one byte late, room when there is none.
try "let take = if incoming < room { incoming } else { room };" "let take = if incoming < room { incoming } else { room + 1 };"
try "(take, incoming >= room)" "(take, incoming > room)"
try "let room = if cap > have { cap - have } else { 0 };" "let room = if cap > have { cap - have } else { 1 };"
# The phase: audio taken while idle or processing, a cancel that leaves the phase, a start taken while a turn runs, a note
# posted that leaves the device busy, an endpoint that does not start the turn, a cancel of nothing that aborts.
try "(Phase::Collecting, Event::Audio) => Step {" "(_, Event::Audio) => Step {"
try "(_, Event::Cancel) => Step {
            next: Phase::Idle," "(_, Event::Cancel) => Step {
            next: p,"
try "            next: p,
            act: Action::RejectBusy," "            next: p,
            act: Action::Listen,"
try "(Phase::Processing, Event::TurnFinished) | (Phase::Processing, Event::NotePosted) => Step {
            next: Phase::Idle," "(Phase::Processing, Event::TurnFinished) | (Phase::Processing, Event::NotePosted) => Step {
            next: Phase::Processing,"
try "(Phase::Collecting, Event::End) | (Phase::Collecting, Event::Endpoint) => Step {
            next: Phase::Processing," "(Phase::Collecting, Event::End) | (Phase::Collecting, Event::Endpoint) => Step {
            next: Phase::Collecting,"
try "(Phase::Idle, Event::Cancel) => Step {
            next: Phase::Idle,
            act: Action::Ignore," "(Phase::Idle, Event::Cancel) => Step {
            next: Phase::Idle,
            act: Action::Abort,"
echo "$refused of $total mutants refused"
exit "$fail"
