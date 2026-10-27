// Protocol 15 (Node retirement) pure kernels: the Telegram delivery outcome, the reminder
// status rule, the Telegram poller lease decision and the update claim. docs/verification.md §15.
//
// Production (branch node-retire: 3c22447; claim.rs and poller.rs from f26eb90). Every exec function below mirrors the code with the
// same rules in the same branch order:
//   classify             <-> crates/lucina-telegram/src/outcome.rs `classify`
//   transport inputs     <-> crates/lucina-telegram/src/lib.rs `BotApi::call` / `transport_error`
//                            (the only callers: (Connect|Sent, status>=400 or None, None) and
//                            (Sent, Some(status), Some(false)); success never calls classify)
//   delivery_outcome     <-> crates/lucina-server/src/reminder_delivery.rs (NotDelivered stays,
//                            every other error is Uncertain, Ok is Delivered)
//   claimed_next         <-> crates/lucina-store/src/reminders.rs `process_due` (match on outcome)
//   status_at_tick_start <-> reminders.rs `status_at_tick_start` (used by `fail_stale_claims`)
//   is_record_live, is_own_previous_run, lease_decision
//                        <-> crates/lucina-telegram/src/lease_decision.rs; the same rules as Node
//                            Lucina/src/gateway-lock.js `isRecordLive`, `isOwnPreviousRun` and the
//                            check in `acquireGatewayLock`
//   should_handle, claim, should_handle_opt, claim_opt, next_offset
//                        <-> crates/lucina-telegram/src/claim.rs (branch node-retire, f26eb90)
//   plan_batch           <-> crates/lucina-telegram/src/poller.rs `plan_batch`
//
// Abstractions: strings are u64 ids, 0 = the empty string (the code only compares them for
// equality and emptiness); `heartbeat_at` is Option<i64> milliseconds (None = not RFC 3339);
// status strings are an enum.
//
// Proven:
//   C1 classify returns Uncertain exactly for (Sent, no status, no API answer). For the inputs
//      the transport passes this is the design's rule "Sent and no HTTP status".
//   C2 Delivered only with api_ok == Some(true); the transport never passes Some(true), so
//      classify never says Delivered for a failed call. NR2: Delivered does not imply a 2xx
//      status (classify(Sent, Some(500), Some(true)) == Delivered); it does under
//      response_consistent.
//   C3 the composed outcome is never NotDelivered after the request was sent without a
//      complete error answer (no retry can duplicate a message Telegram may have).
//   S1 claimed_next: Delivered -> sent, NotDelivered -> pending, Uncertain -> failed.
//   S2 status_at_tick_start: sending -> failed, everything else unchanged.
//   S3 over any legal history of one reminder, once sent or failed it never changes again; in
//      particular a delivered or uncertain send is never followed by another send.
//   L1 lease_decision defers exactly when the record is live, written by another holder and not
//      our own previous run; acquires for no record, a stale or unparseable heartbeat, another
//      boot, the same holder or our own previous run (same instance, or same pid namespace with
//      our pid or a dead pid).
//   L2 two processes with the same instance id never defer to each other's record: between
//      Rust A and a stray Rust B only the flock decides (the Stateright model R3 relies on this).
//   L3 Rust (instance lucina-rs) and Node (lucina-nez, another pid namespace) defer to each
//      other's live record.
//   U1 claims are monotone; a claimed id is never handled again; next_offset(c) = c + 1, so the
//      request after a claim confirms exactly the claimed ids (NR4: also the first one after a start).
//   U3 plan_batch hands out only ids above the old claim, each at most the new claim, strictly
//      increasing, and counts every update of the batch once.
//   U2 at most one handling of an update id over any history of re-deliveries and crashes in
//      which the claim is persisted before handling and never rolled back.
use vstd::prelude::*;

verus! {

// ---- delivery outcome ------------------------------------------------------------------

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Stage { Connect, Sent }

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Outcome { Delivered, NotDelivered, Uncertain }

pub open spec fn classify_spec(stage: Stage, status: Option<u16>, ok: Option<bool>) -> Outcome {
    if ok == Some(true) {
        Outcome::Delivered
    } else if ok == Some(false) || (status is Some && status->Some_0 >= 400) || stage == Stage::Connect {
        Outcome::NotDelivered
    } else if status is None {
        Outcome::Uncertain
    } else {
        Outcome::NotDelivered
    }
}

pub open spec fn is_2xx(status: Option<u16>) -> bool {
    status is Some && 200 <= status->Some_0 < 300
}

/// What Telegram promises: `"ok": true` only on a 2xx answer.
pub open spec fn response_consistent(status: Option<u16>, ok: Option<bool>) -> bool {
    ok == Some(true) ==> is_2xx(status)
}

/// The two shapes of input `BotApi` passes to classify (lib.rs): `transport_error(e, status)`
/// passes `status.filter(|s| *s >= 400)` and `None`; an `ok: false` envelope passes
/// `(Sent, Some(status), Some(false))`.
pub open spec fn transport_input(stage: Stage, status: Option<u16>, ok: Option<bool>) -> bool {
    (ok is None && (status is None || status->Some_0 >= 400))
    || (ok == Some(false) && stage == Stage::Sent && status is Some)
}

/// Mirror of outcome.rs `classify`: the success bit first, then a known failure (an API
/// error, an HTTP error status or a connect-stage error), then a missing answer.
pub fn classify(stage: Stage, http_status: Option<u16>, api_ok: Option<bool>) -> (r: Outcome)
    ensures
        r == classify_spec(stage, http_status, api_ok),
        // C1
        (r == Outcome::Uncertain) <==> (stage == Stage::Sent && http_status is None && api_ok is None),
        transport_input(stage, http_status, api_ok) ==>
            ((r == Outcome::Uncertain) <==> (stage == Stage::Sent && http_status is None)),
        // C2
        r == Outcome::Delivered ==> api_ok == Some(true),
        transport_input(stage, http_status, api_ok) ==> r != Outcome::Delivered,
        response_consistent(http_status, api_ok) && r == Outcome::Delivered ==> is_2xx(http_status),
{
    let ok_true = match api_ok { Some(b) => b, None => false };
    let ok_false = match api_ok { Some(b) => !b, None => false };
    let error_status = match http_status { Some(s) => s >= 400, None => false };
    let connect = match stage { Stage::Connect => true, Stage::Sent => false };
    if ok_true {
        Outcome::Delivered
    } else if ok_false || error_status || connect {
        Outcome::NotDelivered
    } else if http_status.is_none() {
        Outcome::Uncertain
    } else {
        // A complete response did not acknowledge delivery.
        Outcome::NotDelivered
    }
}

/// NR2: the unconditional "Delivered only with a 2xx status" theorem is false for the code.
proof fn nr2_success_bit_wins_over_status()
    ensures
        classify_spec(Stage::Sent, Some(500), Some(true)) == Outcome::Delivered,
        classify_spec(Stage::Connect, None, Some(true)) == Outcome::Delivered,
        !response_consistent(Some(500), Some(true)),
{}

/// The network events of one `sendMessage`, as `BotApi::call` sees them.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Event {
    /// `send()` fails with a connect error (`is_connect`): the request never left.
    ConnectError,
    /// `send()` fails after the request left (timeout, reset): no status.
    SendError,
    /// Headers with `status` arrived, then reading the body failed.
    BodyError { status: u16 },
    /// A body that is not a Telegram envelope (`Invalid Telegram response`).
    Malformed { status: u16 },
    /// A complete envelope.
    Envelope { status: u16, ok: bool },
}

/// Telegram may have the message: everything except a connect error and an error answer
/// (an error envelope, or an error status whose body is missing or not JSON).
pub open spec fn may_be_delivered(e: Event) -> bool {
    match e {
        Event::ConnectError => false,
        Event::Envelope { status, ok } => ok,
        Event::Malformed { status } => status < 400,
        Event::BodyError { status } => status < 400,
        Event::SendError => true,
    }
}

pub open spec fn ok_envelope(e: Event) -> bool {
    match e {
        Event::Envelope { status, ok } => ok,
        _ => false,
    }
}

pub open spec fn call_spec(e: Event) -> Outcome {
    match e {
        Event::ConnectError => classify_spec(Stage::Connect, None, None),
        Event::SendError => classify_spec(Stage::Sent, None, None),
        Event::BodyError { status } =>
            classify_spec(Stage::Sent, if status >= 400 { Some(status) } else { None }, None),
        Event::Malformed { status } => if status >= 400 { Outcome::NotDelivered } else { Outcome::Uncertain },
        Event::Envelope { status, ok } =>
            if ok { Outcome::Delivered } else { classify_spec(Stage::Sent, Some(status), Some(false)) },
    }
}

/// Mirror of `BotApi::call` for one part (lib.rs): the success envelope returns Ok, the error
/// paths call classify as `transport_error` and the envelope check do.
pub fn call_outcome(e: Event) -> (r: Outcome)
    ensures
        r == call_spec(e),
        // C3: never "retry" (NotDelivered) when Telegram may have the message ...
        may_be_delivered(e) ==> r != Outcome::NotDelivered,
        // ... and never "failed, not resent" (Uncertain) after a complete answer or a refusal.
        r == Outcome::Uncertain ==> may_be_delivered(e) && !(e is Envelope),
        r == Outcome::Delivered <==> ok_envelope(e),
{
    match e {
        Event::ConnectError => classify(Stage::Connect, None, None),
        Event::SendError => classify(Stage::Sent, None, None),
        Event::BodyError { status } =>
            classify(Stage::Sent, if status >= 400 { Some(status) } else { None }, None),
        Event::Malformed { status } => if status >= 400 { Outcome::NotDelivered } else { Outcome::Uncertain },
        Event::Envelope { status, ok } =>
            if ok { Outcome::Delivered } else { classify(Stage::Sent, Some(status), Some(false)) },
    }
}

/// reminder_delivery.rs: `send_text` error -> DeliveryOutcome (Ok(_) is Delivered). A
/// multi-part text that fails after an accepted part is Uncertain (`send_text`).
pub fn delivery_outcome(err: Outcome) -> (r: Outcome)
    ensures
        r == (if err == Outcome::NotDelivered { Outcome::NotDelivered } else { Outcome::Uncertain }),
        r != Outcome::Delivered,
{
    match err {
        Outcome::NotDelivered => Outcome::NotDelivered,
        _ => Outcome::Uncertain,
    }
}

// ---- reminder status -------------------------------------------------------------------

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Status { Pending, Sending, Sent, Failed, Cancelled }

pub open spec fn claimed_next_spec(outcome: Outcome) -> Status {
    match outcome {
        Outcome::Delivered => Status::Sent,
        Outcome::NotDelivered => Status::Pending,
        Outcome::Uncertain => Status::Failed,
    }
}

/// process_due: the status a claimed (`sending`) reminder gets from its send outcome (S1).
pub fn claimed_next(outcome: Outcome) -> (r: Status)
    ensures
        r == claimed_next_spec(outcome),
        (r == Status::Pending) <==> (outcome == Outcome::NotDelivered),
        r != Status::Sending,
{
    match outcome {
        Outcome::Delivered => Status::Sent,
        Outcome::NotDelivered => Status::Pending,
        Outcome::Uncertain => Status::Failed,
    }
}

pub open spec fn tick_start_spec(s: Status) -> Status {
    if s == Status::Sending { Status::Failed } else { s }
}

/// reminders.rs `status_at_tick_start` (S2): a claim nobody is sending any more is failed.
pub fn status_at_tick_start(status: Status) -> (r: Status)
    ensures
        r == tick_start_spec(status),
        r != Status::Sending,
        status != Status::Sending ==> r == status,
{
    match status {
        Status::Sending => Status::Failed,
        _ => status,
    }
}

/// One step of a reminder on disk: a claim (pending -> sending, the only step that leads to a
/// send), the outcome of the send, stale-claim recovery, a cancel, or no change.
pub open spec fn legal(a: Status, b: Status) -> bool {
    ||| a == b
    ||| (a == Status::Pending && b == Status::Sending)
    ||| (a == Status::Pending && b == Status::Cancelled)
    ||| (a == Status::Sending && exists|o: Outcome| b == claimed_next_spec(o))
    ||| (a == Status::Sending && b == tick_start_spec(a))
}

pub open spec fn terminal(s: Status) -> bool {
    s == Status::Sent || s == Status::Failed || s == Status::Cancelled
}

proof fn lemma_terminal_step(a: Status, b: Status)
    requires terminal(a), legal(a, b),
    ensures b == a,
{}

/// S3: once terminal, a reminder never changes again, so it is never claimed (sent) again.
proof fn terminal_forever(history: Seq<Status>, k: int)
    requires
        0 <= k < history.len(),
        terminal(history[k]),
        forall|i: int| 0 <= i < history.len() - 1 ==> legal(#[trigger] history[i], history[i + 1]),
    ensures
        forall|i: int| k <= i < history.len() ==> history[i] == history[k],
    decreases history.len() - k,
{
    if k + 1 < history.len() {
        lemma_terminal_step(history[k], history[k + 1]);
        terminal_forever(history, k + 1);
    }
}

/// S3 for a send: after a Delivered or Uncertain outcome (or a stale claim), no later step is a
/// claim, so the reminder is sent at most once more than the NotDelivered retries before it.
proof fn delivered_or_uncertain_never_resent(history: Seq<Status>, k: int, o: Outcome)
    requires
        0 <= k < history.len(),
        o != Outcome::NotDelivered,
        history[k] == claimed_next_spec(o),
        forall|i: int| 0 <= i < history.len() - 1 ==> legal(#[trigger] history[i], history[i + 1]),
    ensures
        forall|i: int| k <= i < history.len() ==> history[i] != Status::Pending && history[i] != Status::Sending,
{
    assert(terminal(history[k]));
    terminal_forever(history, k);
}

// ---- Telegram poller lease -------------------------------------------------------------

pub struct Record {
    pub holder: u64,
    pub instance: u64,
    pub pid_ns: u64,
    pub pid: Option<u32>,
    pub boot_id: u64,
    /// `heartbeatAt` parsed as RFC 3339 milliseconds; None when it does not parse.
    pub heartbeat_ms: Option<i64>,
}

pub struct Identity {
    pub holder: u64,
    pub instance: u64,
    pub pid_ns: u64,
    pub pid: u32,
    /// Snapshot of the record PID's liveness, supplied by the impure caller.
    pub record_pid_alive: bool,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Decision { Acquire, Defer }

pub const STALE_MS: i64 = 90_000;

pub open spec fn live_spec(r: Record, now_ms: i64, boot_id: u64) -> bool {
    &&& r.heartbeat_ms is Some
    &&& now_ms as int - r.heartbeat_ms->Some_0 as int <= STALE_MS as int
    &&& (r.boot_id == 0 || boot_id == 0 || r.boot_id == boot_id)
}

/// lease_decision.rs `is_record_live`. `now_ms.saturating_sub(hb) <= 90_000`: computing the
/// difference in i128 gives the same comparison (saturation only clamps values far beyond the
/// bound); a heartbeat in the future (clock step back) is live, as in Node.
pub fn is_record_live(r: &Record, now_ms: i64, boot_id: u64) -> (b: bool)
    ensures b == live_spec(*r, now_ms, boot_id),
{
    let fresh = match r.heartbeat_ms {
        Some(hb) => (now_ms as i128) - (hb as i128) <= STALE_MS as i128,
        None => false,
    };
    fresh && (r.boot_id == 0 || boot_id == 0 || r.boot_id == boot_id)
}

pub open spec fn own_previous_spec(r: Record, me: Identity) -> bool {
    ||| (me.instance != 0 && r.instance == me.instance)
    ||| (me.pid_ns != 0 && r.pid_ns == me.pid_ns && r.pid is Some
            && (r.pid->Some_0 == me.pid || !me.record_pid_alive))
}

/// lease_decision.rs `is_own_previous_run`.
pub fn is_own_previous_run(r: &Record, me: &Identity) -> (b: bool)
    ensures b == own_previous_spec(*r, *me),
{
    (me.instance != 0 && r.instance == me.instance)
        || (me.pid_ns != 0 && r.pid_ns == me.pid_ns && match r.pid {
            Some(p) => p == me.pid || !me.record_pid_alive,
            None => false,
        })
}

pub open spec fn defer_spec(record: Option<Record>, me: Identity, now_ms: i64, boot_id: u64) -> bool {
    &&& record is Some
    &&& record->Some_0.holder != me.holder
    &&& live_spec(record->Some_0, now_ms, boot_id)
    &&& !own_previous_spec(record->Some_0, me)
}

/// lease_decision.rs `lease_decision` (L1).
pub fn lease_decision(record: Option<&Record>, me: &Identity, now_ms: i64, boot_id: u64) -> (d: Decision)
    ensures
        (d == Decision::Defer) <==> (record is Some && defer_spec(Some(*record->Some_0), *me, now_ms, boot_id)),
        record is None ==> d == Decision::Acquire,
        record is Some && record->Some_0.heartbeat_ms is None ==> d == Decision::Acquire,
        record is Some && record->Some_0.heartbeat_ms is Some
            && now_ms as int - record->Some_0.heartbeat_ms->Some_0 as int > STALE_MS as int ==> d == Decision::Acquire,
        record is Some && record->Some_0.boot_id != 0 && boot_id != 0
            && record->Some_0.boot_id != boot_id ==> d == Decision::Acquire,
        record is Some && record->Some_0.holder == me.holder ==> d == Decision::Acquire,
        record is Some && me.instance != 0 && record->Some_0.instance == me.instance ==> d == Decision::Acquire,
{
    match record {
        Some(r) => {
            if r.holder != me.holder && is_record_live(r, now_ms, boot_id) && !is_own_previous_run(r, me) {
                Decision::Defer
            } else {
                Decision::Acquire
            }
        }
        None => Decision::Acquire,
    }
}

/// L2: Rust A and a stray Rust B (both `HERMES_GATEWAY_INSTANCE` = lucina-rs) never defer to
/// each other's record, however fresh: the record cannot exclude B, only the flock can.
proof fn same_instance_never_defers(r: Record, me: Identity, now_ms: i64, boot_id: u64)
    requires me.instance != 0, r.instance == me.instance,
    ensures !defer_spec(Some(r), me, now_ms, boot_id),
{}

/// L3: a live record written by the other runtime (another instance id, another pid
/// namespace, another holder) makes the reader defer; this holds for Rust reading Node's
/// record and for Node reading Rust's, since gateway-lock.js applies the same rules.
proof fn other_runtime_defers(r: Record, me: Identity, now_ms: i64, boot_id: u64)
    requires
        r.holder != me.holder,
        r.instance != me.instance,
        me.pid_ns != 0, r.pid_ns != me.pid_ns,
        live_spec(r, now_ms, boot_id),
    ensures defer_spec(Some(r), me, now_ms, boot_id),
{}

// ---- update claim ----------------------------------------------------------------------

pub open spec fn handle_spec(update_id: u64, claimed: Option<u64>) -> bool {
    claimed is None || update_id > claimed->Some_0
}

pub open spec fn claim_spec(update_id: u64, claimed: Option<u64>) -> u64 {
    match claimed {
        None => update_id,
        Some(c) => if update_id > c { update_id } else { c },
    }
}

/// claim.rs `should_handle`: `update_id > claimed`.
pub fn should_handle(update_id: u64, claimed: u64) -> (b: bool)
    ensures b == (update_id > claimed),
{
    update_id > claimed
}

/// claim.rs `claim`: `update_id.max(claimed)`.
pub fn claim(update_id: u64, claimed: u64) -> (r: u64)
    ensures r == (if update_id > claimed { update_id } else { claimed }), r >= update_id, r >= claimed,
{
    if update_id > claimed { update_id } else { claimed }
}

/// claim.rs `should_handle_opt`: a missing claim file is no claim.
pub fn should_handle_opt(update_id: u64, claimed: Option<u64>) -> (b: bool)
    ensures b == handle_spec(update_id, claimed),
{
    match claimed {
        None => true,
        Some(c) => should_handle(update_id, c),
    }
}

/// claim.rs `claim_opt` (U1): monotone, and the claimed id is no longer eligible.
pub fn claim_opt(update_id: u64, claimed: Option<u64>) -> (r: u64)
    ensures
        r == claim_spec(update_id, claimed),
        r >= update_id,
        claimed is Some ==> r >= claimed->Some_0,
        !handle_spec(update_id, Some(r)),
{
    match claimed {
        None => update_id,
        Some(c) => claim(update_id, c),
    }
}

/// claim.rs `next_offset`: `c.saturating_add(1)` as i64, `i64::MAX` when it does not fit.
/// The request confirms exactly the ids up to the claim (for any claim below i64::MAX).
/// NR4: poller.rs sends this offset on the first `getUpdates` after a start too.
pub fn next_offset(claimed: Option<u64>) -> (r: i64)
    ensures
        claimed is None ==> r == 0,
        claimed is Some ==> r as int == if claimed->Some_0 as int + 1 <= i64::MAX as int {
            claimed->Some_0 as int + 1 } else { i64::MAX as int },
        claimed is Some && claimed->Some_0 < i64::MAX as u64 ==> r as int == claimed->Some_0 as int + 1,
{
    match claimed {
        None => 0,
        Some(c) => {
            let n: u64 = if c == u64::MAX { u64::MAX } else { c + 1 };
            if n <= i64::MAX as u64 { n as i64 } else { i64::MAX }
        }
    }
}

/// poller.rs `plan_batch` over the batch's `update_id`s (None = an update without one): the
/// updates to hand out in order, the new claim (persisted before any is dispatched), and the
/// skipped / malformed counts. Proven: every handed-out id is above the old claim and at most
/// the new one, the handed-out ids strictly increase (no id twice in a batch), the claim never
/// goes down, and every update is counted once.
pub fn plan_batch(ids: &Vec<Option<u64>>, claimed: Option<u64>) -> (r: (Vec<u64>, Option<u64>, u64, u64))
    requires ids.len() < u64::MAX,
    ensures
        r.0.len() + r.2 + r.3 == ids.len(),
        claimed is Some ==> (r.1 is Some && r.1->Some_0 >= claimed->Some_0),
        r.0.len() > 0 ==> r.1 is Some,
        forall|k: int| 0 <= k < r.0.len() ==> handle_spec(#[trigger] r.0[k], claimed) && r.0[k] <= r.1->Some_0,
        forall|a: int, b: int| 0 <= a < b < r.0.len() ==> r.0[a] < r.0[b],
{
    let mut handle: Vec<u64> = Vec::new();
    let mut c = claimed;
    let mut skipped: u64 = 0;
    let mut malformed: u64 = 0;
    let mut i: usize = 0;
    while i < ids.len()
        invariant
            ids.len() < u64::MAX,
            0 <= i <= ids.len(),
            handle.len() + skipped + malformed == i,
            claimed is Some ==> (c is Some && c->Some_0 >= claimed->Some_0),
            handle.len() > 0 ==> c is Some,
            forall|k: int| 0 <= k < handle.len() ==> handle_spec(#[trigger] handle[k], claimed) && handle[k] <= c->Some_0,
            forall|a: int, b: int| 0 <= a < b < handle.len() ==> handle[a] < handle[b],
            handle.len() > 0 ==> handle[handle.len() - 1] == c->Some_0,
        decreases ids.len() - i,
    {
        match ids[i] {
            None => {
                malformed = malformed + 1;
            }
            Some(id) => {
                if should_handle_opt(id, c) {
                    let n = claim_opt(id, c);
                    c = Some(n);
                    handle.push(id);
                } else {
                    skipped = skipped + 1;
                }
            }
        }
        i = i + 1;
    }
    (handle, c, skipped, malformed)
}

proof fn claim_chain(c: Seq<Option<u64>>, i: int, j: int)
    requires
        0 <= i <= j < c.len(),
        c[i] is Some,
        forall|k: int| 0 <= k < c.len() - 1 ==> (#[trigger] c[k] is Some ==> c[k + 1] is Some && c[k + 1]->Some_0 >= c[k]->Some_0),
    ensures c[j] is Some && c[j]->Some_0 >= c[i]->Some_0,
    decreases j - i,
{
    if i < j {
        assert(c[i + 1] is Some && c[i + 1]->Some_0 >= c[i]->Some_0);
        claim_chain(c, i + 1, j);
    }
}

/// U2: deliveries ids[0..n] (any order, any repetition: Telegram re-delivers unconfirmed
/// updates after a crash); claims[k] is the durable claim when delivery k is examined.
/// A handled delivery was eligible and is claimed before its handler runs; a crash may lose
/// the handler but never rolls the claim back. Then no update id is handled twice.
proof fn handled_at_most_once(ids: Seq<u64>, claims: Seq<Option<u64>>, handled: Seq<bool>, i: int, j: int)
    requires
        claims.len() == ids.len() + 1,
        handled.len() == ids.len(),
        0 <= i < j < ids.len(), ids[i] == ids[j], handled[i], handled[j],
        forall|k: int| 0 <= k < ids.len() ==> (#[trigger] handled[k] ==> handle_spec(ids[k], claims[k])),
        forall|k: int| 0 <= k < ids.len() ==> (#[trigger] handled[k] ==> claims[k + 1] is Some && claims[k + 1]->Some_0 >= ids[k]),
        forall|k: int| 0 <= k < claims.len() - 1 ==> (#[trigger] claims[k] is Some ==> claims[k + 1] is Some && claims[k + 1]->Some_0 >= claims[k]->Some_0),
    ensures false,
{
    assert(claims[i + 1] is Some && claims[i + 1]->Some_0 >= ids[i]);
    claim_chain(claims, i + 1, j);
    assert(claims[j] is Some && claims[j]->Some_0 >= ids[j]);
    assert(handle_spec(ids[j], claims[j]));
}

fn witnesses() {
    let u = classify(Stage::Sent, None, None);
    assert(u == Outcome::Uncertain);
    let n = classify(Stage::Sent, Some(429), Some(false));
    assert(n == Outcome::NotDelivered);
    let c = classify(Stage::Connect, None, None);
    assert(c == Outcome::NotDelivered);
    let b = call_outcome(Event::BodyError { status: 200 });
    assert(b == Outcome::Uncertain);
    let st = claimed_next(delivery_outcome(u));
    assert(st == Status::Failed);
    let p = claimed_next(delivery_outcome(n));
    assert(p == Status::Pending);
    let f = status_at_tick_start(Status::Sending);
    assert(f == Status::Failed);
    let node = Record { holder: 1, instance: 10, pid_ns: 20, pid: Some(7), boot_id: 5, heartbeat_ms: Some(1_000) };
    let rust = Identity { holder: 2, instance: 11, pid_ns: 21, pid: 7, record_pid_alive: true };
    let d = lease_decision(Some(&node), &rust, 91_000, 5);
    assert(d == Decision::Defer);
    let stale = lease_decision(Some(&node), &rust, 91_001, 5);
    assert(stale == Decision::Acquire);
    let k = claim_opt(7, None);
    let again = should_handle_opt(7, Some(k));
    assert(!again);
    let o = next_offset(Some(k));
    assert(o == 8);
    let mut batch: Vec<Option<u64>> = Vec::new();
    batch.push(Some(5));
    batch.push(None);
    batch.push(Some(6));
    batch.push(Some(6));
    let (h, c2, skipped, malformed) = plan_batch(&batch, Some(5));
    assert(h.len() + skipped + malformed == 4);
    assert(c2 is Some && c2->Some_0 >= 5);
}

fn main() {
}

} // verus!
