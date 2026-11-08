// The rules under the LucinaVoice socket's streaming input (`docs/lucinavoice-protocol.md` section 10) that must hold
// whatever the client sends: IMA-ADPCM's step and predictor, the block maths, the block assembler, the resampler, the
// recording's byte cap and the phase of a connection's turn. Verus checks this very file: `//@` lines hold the proof, see
// scripts/verify-shipped.sh (D-011, as in LucinaOS). Mutants that Verus must refuse: verification/voice-frames/mutants.sh.
// `voice_frames.rs` re-exports these items and builds the WAV headers and the `start` validation on top of them.
//
// The promises, all proved below:
// - ADPCM: whatever the state and the nibble, the step index stays in 0..=88, the predictor stays a 16-bit sample, and
//   every table index is in bounds (no panic, no overflow: the change is computed in 64 bits).
// - Block maths: a block of `block` bytes (4 and up) holds `2 * (block - 4) + 1` samples; a partial block of `n >= 4`
//   bytes yields `2 * (n - 4) + 1`; fewer than 4 bytes yield none. `decode_block` pushes exactly that many samples.
// - The assembler never holds `block` bytes or more between calls, and never reads outside a frame or its buffer.
// - The resampler: every read of the input is in bounds, its state stays in range, and a call that adds `k` samples for
//   `m` input samples moves the fractional position `pos` by `k * rate = m * 16000 + pos' - pos`, with
//   `0 < pos' <= rate`. Summed over a stream of `N` input samples that is exactly `K = floor((N - 1) * 16000 / rate) + 1`
//   output samples: within `1` above and `16000 / rate` (at most 2) below `N * 16000 / rate`.
// - The byte cap: a recording never holds more than its cap, and it is full exactly when the chunk did not fit.
// - The phase machine: one turn at a time (a `start` is only taken from Idle), audio is taken only while collecting,
//   `cancel` always ends in Idle, and after a note is posted the phase is Idle.

//@ global size_of usize == 8;

/// Smallest and largest `block` a client may announce.
pub const MIN_BLOCK: u32 = 8;
pub const MAX_BLOCK: u32 = 4096;
/// The rate the server works at (the VAD, the speech-to-text and the 16 kHz WAV).
pub const OUT_RATE: u32 = 16_000;
/// The resampler accepts 8000 to 48000.
pub const MIN_RATE: u32 = 8_000;
pub const MAX_RESAMPLE_RATE: u32 = 48_000;

/// The standard IMA-ADPCM step table.
pub const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408,
    449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066,
    2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630,
    9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];

/// The step index stays within its table.
pub fn clamp_index(i: i32) -> i32 //@ret
//@ ensures 0 <= ret <= 88, (0 <= i <= 88) ==> ret == i, i < 0 ==> ret == 0, i > 88 ==> ret == 88,
{
    if i < 0 {
        0
    } else if i > 88 {
        88
    } else {
        i
    }
}

/// A predicted sample stays a 16-bit sample.
pub fn clamp_sample(v: i32) -> i32 //@ret
//@ ensures -32768 <= ret <= 32767, (-32768 <= v <= 32767) ==> ret == v, v < -32768 ==> ret == -32768, v > 32767 ==> ret == 32767,
{
    if v < -32768 {
        -32768
    } else if v > 32767 {
        32767
    } else {
        v
    }
}

/// Like [`clamp_sample`], for the 64-bit sum the predictor is computed in.
fn clamp_sample64(v: i64) -> i32 //@ret
//@ ensures -32768 <= ret <= 32767, (-32768 <= v <= 32767) ==> ret == v, v < -32768 ==> ret == -32768, v > 32767 ==> ret == 32767,
{
    if v < -32768 {
        -32768
    } else if v > 32767 {
        32767
    } else {
        v as i32
    }
}

/// The step index change of a 4-bit code (the table 0..7 twice: -1 for the small magnitudes, then 2, 4, 6, 8).
pub fn index_delta(code: u8) -> i32 //@ret
//@ ensures -1 <= ret <= 8, code % 8 < 4 ==> ret == -1,
{
    match code % 8 {
        4 => 2,
        5 => 4,
        6 => 6,
        7 => 8,
        _ => -1,
    }
}

/// The predictor and the step index after one 4-bit code (ffmpeg's `adpcm_ima_wav`: the change is
/// `((2 * magnitude + 1) * step) / 8` of the step the index names, the sign is bit 3). Any state and any code give a state
/// in range: the sum is 64-bit, so nothing overflows, and both results are clamped.
pub fn expand(pred: i32, index: i32, code: u8) -> (i32, i32) //@ret
//@ ensures -32768 <= ret.0 <= 32767, 0 <= ret.1 <= 88,
{
    let index = clamp_index(index);
    let step = STEP_TABLE[index as usize] as i64;
    let magnitude = (code % 8) as i64;
    //@ assert(-(15 * 2147483648) <= (2 * magnitude + 1) * step <= 15 * 2147483648) by (nonlinear_arith) requires 0 <= magnitude <= 7, -2147483648 <= step <= 2147483647;
    let diff = ((2 * magnitude + 1) * step) / 8;
    let next = if code % 16 >= 8 {
        pred as i64 - diff
    } else {
        pred as i64 + diff
    };
    (clamp_sample64(next), clamp_index(index + index_delta(code)))
}

//@ pub open spec fn spb(n: int) -> int { if n < 4 { 0 } else { 2 * (n - 4) + 1 } }

/// Samples in one whole block of `block` bytes: `2 * (block - 4) + 1`.
pub fn samples_per_block(block: usize) -> usize //@ret
//@ requires block <= 0xffff_ffff,
//@ ensures ret as int == spb(block as int), 4 <= block ==> ret as int == 2 * (block as int - 4) + 1, block < 4 ==> ret == 0,
{
    if block < 4 {
        0
    } else {
        2 * (block - 4) + 1
    }
}

/// Samples in a partial last block of `len` bytes: the codes it has, or none under 4 bytes.
pub fn partial_samples(len: usize) -> usize //@ret
//@ requires len <= 0xffff_ffff,
//@ ensures ret as int == spb(len as int), 4 <= len ==> ret as int == 2 * (len as int - 4) + 1, len < 4 ==> ret == 0,
{
    samples_per_block(len)
}

/// Samples in `data_len` bytes of blocks of `block` bytes.
pub fn total_samples(data_len: usize, block: usize) -> usize //@ret
//@ requires data_len <= 0xffff_ffff, block <= 4096,
//@ ensures block == 0 ==> ret == 0, block > 0 ==> ret as int == (data_len as int / block as int) * spb(block as int) + spb(data_len as int % block as int),
{
    if block == 0 {
        return 0;
    }
    let whole = data_len / block;
    let rest = data_len % block;
    //@ assert(whole as int * block as int <= data_len as int) by (nonlinear_arith) requires whole as int == data_len as int / block as int, block > 0;
    //@ assert(whole as int * spb(block as int) <= 2 * data_len as int) by (nonlinear_arith) requires whole as int * block as int <= data_len as int, block > 0, block <= 4096, spb(block as int) <= 2 * block as int, whole >= 0;
    whole * samples_per_block(block) + partial_samples(rest)
}

/// Decode `blk[start..end]` (one block, or a partial one) of mono IMA-ADPCM into `out`. Fewer than 4 bytes decode to
/// nothing. Bytes 0-1 are the first sample, byte 2 the step index (above 88 it is 88), byte 3 is ignored; then two codes
/// per byte, low nibble first. Exactly `spb(end - start)` samples are added.
pub fn decode_block_range(blk: &[u8], start: usize, end: usize, out: &mut Vec<i16>)
//@ requires start <= end <= blk@.len(),
//@ ensures final(out)@.len() == old(out)@.len() + spb((end - start) as int),
{
    if end - start < 4 {
        return;
    }
    let raw = blk[start + 1] as i32 * 256 + blk[start] as i32;
    let mut pred = if raw >= 32768 { raw - 65536 } else { raw };
    let mut index = clamp_index(blk[start + 2] as i32);
    out.push(pred as i16);
    let mut i = start + 4;
    while i < end
    //@ invariant start + 4 <= i <= end, end <= blk@.len(), -32768 <= pred <= 32767, 0 <= index <= 88,
    //@     out@.len() == old(out)@.len() + 1 + 2 * (i - start - 4),
    //@ decreases end - i,
    {
        let byte = blk[i];
        let (p, x) = expand(pred, index, byte % 16);
        pred = p;
        index = x;
        out.push(pred as i16);
        let (p, x) = expand(pred, index, byte / 16);
        pred = p;
        index = x;
        out.push(pred as i16);
        i += 1;
    }
}

/// [`decode_block_range`] of a whole slice.
pub fn decode_block(blk: &[u8], out: &mut Vec<i16>)
//@ ensures final(out)@.len() == old(out)@.len() + spb(blk@.len() as int),
{
    decode_block_range(blk, 0, blk.len(), out);
}

/// Cuts a byte stream (frames split anywhere) into blocks and decodes each completed one. Fewer
/// than `block` bytes are ever kept.
#[derive(Debug, Clone)] //@omit
pub struct AdpcmDecoder {
    block: usize,
    buf: Vec<u8>,
}

impl AdpcmDecoder {
    //@ pub closed spec fn wf(&self) -> bool { 8 <= self.block <= 4096 && self.buf@.len() < self.block }
    //@ pub closed spec fn held(&self) -> int { self.buf@.len() as int }
    //@ pub closed spec fn size(&self) -> int { self.block as int }

    pub fn new(block: u32) -> Self //@ret
    //@ ensures ret.wf(), ret.held() == 0, ret.size() == (if block < 8 { 8 } else if block > 4096 { 4096 } else { block as int }),
    {
        let block = if block < MIN_BLOCK {
            MIN_BLOCK
        } else if block > MAX_BLOCK {
            MAX_BLOCK
        } else {
            block
        };
        Self {
            block: block as usize,
            buf: Vec::new(),
        }
    }

    /// Bytes waiting for the rest of their block: always fewer than `block`.
    pub fn buffered(&self) -> usize //@ret
    //@ requires self.wf(),
    //@ ensures ret < self.size(), ret == self.held(),
    {
        self.buf.len()
    }

    pub fn push(&mut self, data: &[u8], out: &mut Vec<i16>)
    //@ requires old(self).wf(),
    //@ ensures final(self).wf(), final(self).size() == old(self).size(),
    {
        let mut i = 0;
        while i < data.len()
        //@ invariant self.wf(), self.size() == old(self).size(), i <= data.len(),
        //@ decreases data.len() - i,
        {
            if self.buf.len() == 0 && data.len() - i >= self.block {
                // A whole block in this frame: decode it in place.
                decode_block_range(data, i, i + self.block, out);
                i += self.block;
            } else {
                self.buf.push(data[i]);
                i += 1;
                if self.buf.len() >= self.block {
                    decode_block_range(self.buf.as_slice(), 0, self.buf.len(), out);
                    self.buf.clear();
                }
            }
        }
    }

    /// The end of the recording: a partial last block of at least 4 bytes decodes for the codes
    /// it has; under 4 bytes it is dropped.
    pub fn finish(&mut self, out: &mut Vec<i16>)
    //@ requires old(self).wf(),
    //@ ensures final(self).wf(), final(self).held() == 0, final(out)@.len() == old(out)@.len() + spb(old(self).held()),
    {
        if self.buf.len() >= 4 {
            decode_block_range(self.buf.as_slice(), 0, self.buf.len(), out);
        }
        self.buf.clear();
    }
}

/// Linear resampler from `rate` to [`OUT_RATE`], streaming: it carries the fractional position
/// and the last sample across calls. Output number `j` sits at input position `j * rate / 16000`.
#[derive(Debug, Clone)] //@omit
pub struct Resampler {
    rate: u32,
    last: i32,
    started: bool,
    /// The next output's position, in 1/16000 input samples after `last`.
    pos: u64,
}

impl Resampler {
    //@ pub closed spec fn wf(&self) -> bool {
    //@     8000 <= self.rate <= 48000 && (!self.started ==> self.pos == 0)
    //@     && (self.started ==> 0 < self.pos <= self.rate) && -32768 <= self.last <= 32767
    //@ }
    //@ pub closed spec fn rate_of(&self) -> int { self.rate as int }
    //@ pub closed spec fn pos_of(&self) -> int { self.pos as int }
    //@ pub closed spec fn is_started(&self) -> bool { self.started }

    pub fn new(rate: u32) -> Self //@ret
    //@ ensures ret.wf(), ret.rate_of() == (if rate < 8000 { 8000 } else if rate > 48000 { 48000 } else { rate as int }),
    {
        let rate = if rate < MIN_RATE {
            MIN_RATE
        } else if rate > MAX_RESAMPLE_RATE {
            MAX_RESAMPLE_RATE
        } else {
            rate
        };
        Self {
            rate,
            last: 0,
            started: false,
            pos: 0,
        }
    }

    /// Resample `input`, appending to `out`. For `m` samples taken in (all of them, except the very first of the stream,
    /// which only starts it) and `k` samples added: `k * rate + old pos == m * 16000 + new pos`, and the new `pos` is in
    /// `1..=rate`. So `N` samples in give exactly `floor((N - 1) * 16000 / rate) + 1` out.
    pub fn push(&mut self, input: &[i16], out: &mut Vec<i16>)
    //@ requires old(self).wf(), input@.len() <= 0xffff_ffff,
    //@ ensures final(self).wf(), final(self).rate_of() == old(self).rate_of(),
    //@     input@.len() == 0 ==> (final(out)@.len() == old(out)@.len() && final(self).pos_of() == old(self).pos_of() && final(self).is_started() == old(self).is_started()),
    //@     input@.len() > 0 ==> (final(self).is_started()
    //@         && (final(out)@.len() - old(out)@.len()) * final(self).rate_of() + old(self).pos_of()
    //@             == (if old(self).is_started() { input@.len() as int } else { input@.len() as int - 1 }) * 16000 + final(self).pos_of()),
    //@     (!old(self).is_started() && input@.len() > 0) ==> final(out)@.len() > old(out)@.len(),
    {
        if input.len() == 0 {
            return;
        }
        let step: u64 = self.rate as u64;
        let n = input.len();
        let mut start: usize = 0;
        if !self.started {
            self.started = true;
            self.last = input[0] as i32;
            self.pos = 0;
            start = 1;
        }
        // The samples seen are e[0] = last, e[k] = input[start + k - 1] for k in 1..=m.
        let m: u64 = (n - start) as u64;
        //@ let ghost old_len = out@.len();
        //@ let ghost old_pos = self.pos as int;
        //@ let ghost old_started = old(self).is_started();
        // An output at position `pos` is made while `pos <= m * 16000`: its input sample `pos / 16000` is then at most
        // `m`, and its right neighbour (needed unless `pos` is a whole sample) is before `m`.
        while self.pos <= m * 16000
        //@ invariant self.rate_of() == old(self).rate_of(), 8000 <= self.rate <= 48000, self.rate as u64 == step,
        //@     n == input@.len(), 0 < n <= 0xffff_ffff, m == (n - start) as u64, start <= 1,
        //@     old_started ==> start == 0, !old_started ==> start == 1,
        //@     -32768 <= self.last <= 32767, old_pos <= step, !old_started ==> old_pos == 0,
        //@     self.pos as int == old_pos + (out@.len() - old_len) * step as int,
        //@     out@.len() >= old_len,
        //@     out@.len() == old_len ==> self.pos as int == old_pos,
        //@     out@.len() > old_len ==> self.pos <= m * 16000 + step,
        //@     self.started,
        //@ decreases m * 16000 + 48000 - self.pos,
        {
            let idx = self.pos / 16000;
            let frac = self.pos % 16000;
            let a: i32 = if idx == 0 {
                self.last
            } else {
                input[start + idx as usize - 1] as i32
            };
            let v: i32 = if frac == 0 {
                a
            } else {
                let b = input[start + idx as usize] as i32;
                //@ assert(-(65535 * 16000) <= (b as i64 - a as i64) * frac as i64 <= 65535 * 16000) by (nonlinear_arith) requires -32768 <= a <= 32767, -32768 <= b <= 32767, 0 <= frac < 16000;
                let d = (b as i64 - a as i64) * frac as i64 / 16000;
                clamp_sample64(a as i64 + d)
            };
            out.push(v as i16);
            //@ assert((out@.len() - old_len) * step as int == (out@.len() - 1 - old_len) * step as int + step as int) by (nonlinear_arith);
            self.pos += step;
        }
        if m > 0 {
            self.pos -= m * 16000;
            self.last = input[input.len() - 1] as i32;
        }
    }
}

// -- the recording's cap -------------------------------------------------------------------------------------------------

/// How much of an incoming chunk of `incoming` bytes the recording keeps when it already holds
/// `have` bytes and may hold `cap`; and whether the cap is reached (the turn then runs).
pub fn audio_room(have: usize, incoming: usize, cap: usize) -> (usize, bool) //@ret
//@ ensures ret.0 <= incoming, have <= cap ==> have + ret.0 <= cap, have > cap ==> ret.0 == 0,
//@     ret.1 == (incoming >= (if cap > have { cap - have } else { 0 })),
//@     ret.1 == false ==> ret.0 == incoming,
//@     ret.1 == true ==> (have <= cap ==> have + ret.0 == cap) || ret.0 == incoming,
{
    let room = if cap > have { cap - have } else { 0 };
    let take = if incoming < room { incoming } else { room };
    (take, incoming >= room)
}

// -- the turn phase --------------------------------------------------------------------------------------------------------

/// Where one connection's turn is.
#[derive(Debug, PartialEq, Eq)] //@omit
#[derive(Clone, Copy)]
pub enum Phase {
    /// No turn: a `start` is accepted.
    Idle,
    /// A turn is listening: frames are audio.
    Collecting,
    /// The recording is complete and the turn runs (speech-to-text, chat, speech, or the note's
    /// upload). The device's slot is held.
    Processing,
}

/// What happened.
#[derive(Debug, PartialEq, Eq)] //@omit
#[derive(Clone, Copy)]
pub enum Event {
    /// A well-formed `start` (checked in order: this first, then its keys).
    Start,
    /// A binary frame.
    Audio,
    /// The text `end`.
    End,
    /// The text `cancel`.
    Cancel,
    /// The VAD found the end of speech, or the byte cap was reached.
    Endpoint,
    /// The task of the turn ended.
    TurnFinished,
    /// The note's share was accepted: the device is free before the answer comes.
    NotePosted,
}

/// What the socket does.
#[derive(Debug, PartialEq, Eq)] //@omit
#[derive(Clone, Copy)]
pub enum Action {
    /// Nothing (a late frame, `end` or `cancel` with nothing to end).
    Ignore,
    /// Begin listening (the caller still has to check the keys, the rate limit and the device).
    Listen,
    /// A `start` while a turn runs: `busy`, nothing changes.
    RejectBusy,
    /// Take the frame into the recording.
    Accept,
    /// The recording is complete: run the turn.
    Process,
    /// Drop the turn in progress (its task is aborted, its slot freed).
    Abort,
    /// The turn is over: free the slot.
    Release,
}

#[derive(Debug, PartialEq, Eq)] //@omit
#[derive(Clone, Copy)]
pub struct Step {
    pub next: Phase,
    pub act: Action,
}

/// The one transition function. One turn at a time; audio only while collecting; `cancel` always
/// ends in `Idle`; a note leaves `Processing` for `Idle` when it is posted.
pub fn step(p: Phase, e: Event) -> Step //@ret
//@ ensures
//@     // One turn at a time: a start is taken from Idle only; anywhere else it is busy and changes nothing.
//@     (e == Event::Start && p == Phase::Idle) ==> (ret.act == Action::Listen && ret.next == Phase::Collecting),
//@     (e == Event::Start && p != Phase::Idle) ==> (ret.act == Action::RejectBusy && ret.next == p),
//@     ret.act == Action::Listen ==> (p == Phase::Idle && e == Event::Start),
//@     // Audio is taken only while collecting, and then the phase stays; otherwise it is ignored and nothing changes.
//@     (ret.act == Action::Accept) ==> (p == Phase::Collecting && e == Event::Audio && ret.next == Phase::Collecting),
//@     (p == Phase::Collecting && e == Event::Audio) ==> ret.act == Action::Accept,
//@     (p != Phase::Collecting && e == Event::Audio) ==> (ret.act == Action::Ignore && ret.next == p),
//@     // A cancel always ends in Idle; it aborts what ran and ignores nothing-to-cancel.
//@     e == Event::Cancel ==> ret.next == Phase::Idle,
//@     (e == Event::Cancel && p == Phase::Idle) ==> ret.act == Action::Ignore,
//@     (e == Event::Cancel && p != Phase::Idle) ==> ret.act == Action::Abort,
//@     ret.act == Action::Abort ==> (e == Event::Cancel && p != Phase::Idle),
//@     // After a note is posted the phase is Idle, and the slot is released.
//@     (e == Event::NotePosted && p == Phase::Processing) ==> (ret.next == Phase::Idle && ret.act == Action::Release),
//@     // The turn runs only from Collecting, after the end or an endpoint, and then it is processing.
//@     ret.act == Action::Process ==> (p == Phase::Collecting && (e == Event::End || e == Event::Endpoint) && ret.next == Phase::Processing),
//@     (p == Phase::Collecting && (e == Event::End || e == Event::Endpoint)) ==> ret.act == Action::Process,
//@     // The slot is released only when a turn was processing.
//@     ret.act == Action::Release ==> (p == Phase::Processing && (e == Event::TurnFinished || e == Event::NotePosted) && ret.next == Phase::Idle),
//@     // A phase changes only by these events.
//@     ret.next != p ==> (
//@         (p == Phase::Idle && e == Event::Start)
//@         || (p == Phase::Collecting && (e == Event::End || e == Event::Endpoint || e == Event::Cancel))
//@         || (p == Phase::Processing && (e == Event::Cancel || e == Event::TurnFinished || e == Event::NotePosted))),
{
    match (p, e) {
        (Phase::Idle, Event::Start) => Step {
            next: Phase::Collecting,
            act: Action::Listen,
        },
        (_, Event::Start) => Step {
            next: p,
            act: Action::RejectBusy,
        },
        (Phase::Collecting, Event::Audio) => Step {
            next: p,
            act: Action::Accept,
        },
        (Phase::Collecting, Event::End) | (Phase::Collecting, Event::Endpoint) => Step {
            next: Phase::Processing,
            act: Action::Process,
        },
        (Phase::Idle, Event::Cancel) => Step {
            next: Phase::Idle,
            act: Action::Ignore,
        },
        (_, Event::Cancel) => Step {
            next: Phase::Idle,
            act: Action::Abort,
        },
        (Phase::Processing, Event::TurnFinished) | (Phase::Processing, Event::NotePosted) => Step {
            next: Phase::Idle,
            act: Action::Release,
        },
        _ => Step {
            next: p,
            act: Action::Ignore,
        },
    }
}
