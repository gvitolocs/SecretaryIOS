---------------------------- MODULE OtpForwarding ----------------------------
(***************************************************************************)
(* Lucina OTP forwarding (branch lucina-rs): the phone core                *)
(* (crates/lucina-android bridge.rs), the relay on p1                      *)
(* (crates/lucina-otp-relay store.rs + http.rs) and the owner's /otp       *)
(* through Lucina (crates/lucina-server otp.rs). TLA+ twin of              *)
(* verification/models/src/otp.rs: the same actions, switches and          *)
(* properties.                                                             *)
(*                                                                         *)
(* Code 1 arrives as an SMS and its notification, code 2 as an SMS; each   *)
(* copy within the code's life, stamped with its own arrival time. The     *)
(* phone handles one copy at a time (the JNI mutex): dedup by code within  *)
(* TTL, a fresh event id per forwarded copy (reused on every retry), the   *)
(* durable queue. state.bin (the dedup set) is saved after the first send  *)
(* returns. A reboot keeps the queue and the saved state and loses the     *)
(* request in flight. The network loses a request or a response. The      *)
(* relay keeps pending codes (newest out first, consumed once) and seen    *)
(* event ids in memory, plus the paused file. Lucina answers /otp in       *)
(* steps: take; on 204 status; arm when not armed; it checks the expiry    *)
(* of a taken code before showing it.                                      *)
(*                                                                         *)
(* WriteAhead, ArmKeepsCodes, PersistSeen are the proposed fixes F1-F3     *)
(* (FALSE = the current code); OwnerGuard, LucinaExpiryCheck, RelayDedup   *)
(* are mechanisms of the current code (ablations switch one off).          *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS TTL, MaxTime, Continuous, WindowLen, RelayLimit, QueueLimit,
          Drops, Crashes, Restarts, Takes, Threads, Stops, Arms, Tests, Others,
          WriteAhead, ArmKeepsCodes, PersistSeen,
          OwnerGuard, LucinaExpiryCheck, RelayDedup

Copies == << <<"sms", "notification">>, <<"sms">> >>
Codes == 1..Len(Copies)
TestCode == Len(Copies) + 1
None == -1

VARIABLES now, born, arrived,
          queue, recent, recentDisk, inflight,
          window, paused, pending, seen,
          lucina,
          drops, crashes, restarts, takes, stops, arms, tests, others,
          delivered, exempt, preCrash, leaked, retryDedup

env == <<now, born, arrived>>
phone == <<queue, recent, recentDisk, inflight>>
relay == <<window, paused, pending, seen>>
budget == <<drops, crashes, restarts, takes, stops, arms, tests, others>>
ghost == <<delivered, exempt, preCrash, leaked, retryDedup>>
vars == <<env, phone, relay, lucina, budget, ghost>>

NoEntry == [code |-> 0, eid |-> <<0, 0>>, recv |-> 0, src |-> "sms"]
NoFlight == [on |-> FALSE, e |-> NoEntry, kind |-> "none", stage |-> "none", out |-> "none"]
NoWait == [code |-> 0, exp |-> 0, src |-> "sms"]
Off == [k |-> "off", end |-> 0]
Cont == [k |-> "cont", end |-> 0]
Until(e) == [k |-> "until", end |-> e]

Min(a, b) == IF a < b THEN a ELSE b
Armed(w) == w.k = "cont" \/ (w.k = "until" /\ now < w.end)
CodesOf(s) == {s[i].code : i \in 1..Len(s)}
CodesWhere(s, P(_)) == {s[i].code : i \in {j \in 1..Len(s) : P(s[j])}}
Fresh(x, t) == x.recv + TTL > t
Remove(s, i) == [j \in 1..(Len(s) - 1) |-> IF j < i THEN s[j] ELSE s[j + 1]]

(* Store::receive -> [out, pending, seen, ex] *)
Receive(e) ==
    LET live == SelectSeq(pending, LAMBDA w : w.exp > now)
        gone == CodesWhere(pending, LAMBDA w : w.exp <= now)
        seen1 == {x \in seen : x[2] > now}
        dup == RelayDedup /\ \E x \in seen1 : x[1] = e.eid
        exp0 == Min(e.recv, now) + TTL
        exp == IF window.k = "until" THEN Min(exp0, window.end) ELSE exp0
        full == Len(live) >= RelayLimit
        kept == IF full THEN Tail(live) ELSE live
        evicted == IF full THEN {Head(live).code} ELSE {}
    IN CASE e.code = TestCode ->
                [out |-> "testack", pending |-> pending, seen |-> seen, ex |-> {}]
         [] ~Armed(window) ->
                [out |-> "paused", pending |-> pending, seen |-> seen, ex |-> {}]
         [] dup ->
                [out |-> "duplicate", pending |-> live, seen |-> seen1, ex |-> gone]
         [] exp <= now ->
                [out |-> "expired", pending |-> live, seen |-> seen1, ex |-> gone]
         [] OTHER ->
                [out |-> "accepted",
                 pending |-> Append(kept, [code |-> e.code, exp |-> exp, src |-> e.src]),
                 seen |-> IF RelayDedup THEN seen1 \cup {<<e.eid, now + TTL>>} ELSE seen1,
                 ex |-> gone \cup evicted]

(* Store::take -> [has, got, pending, window, ex] *)
RECURSIVE PopFresh(_, _)
PopFresh(p, ex) ==
    IF p = <<>> THEN [has |-> FALSE, got |-> NoWait, pending |-> p, ex |-> ex]
    ELSE LET w == p[Len(p)]
             rest == SubSeq(p, 1, Len(p) - 1)
         IN IF w.exp > now THEN [has |-> TRUE, got |-> w, pending |-> rest, ex |-> ex]
            ELSE PopFresh(rest, ex \cup {w.code})
Take ==
    IF ~Armed(window)
    THEN [has |-> FALSE, got |-> NoWait, pending |-> <<>>, window |-> Off, ex |-> CodesOf(pending)]
    ELSE LET r == PopFresh(pending, {})
         IN [has |-> r.has, got |-> r.got, pending |-> r.pending, window |-> window, ex |-> r.ex]

(* Store::arm -> [pending, window, paused, ex]. The current code drops the  *)
(* waiting codes even when armed (not exempt); F2 keeps them.               *)
ArmR ==
    LET was == Armed(window)
        keep == ArmKeepsCodes /\ was
    IN [pending |-> IF keep THEN pending ELSE <<>>,
        ex |-> IF ~keep /\ ~was THEN CodesOf(pending) ELSE {},
        window |-> IF Continuous THEN Cont ELSE Until(now + WindowLen),
        paused |-> IF Continuous THEN FALSE ELSE paused]

(* Bridge::enqueue -> [queue, ex] *)
Enqueue(q, e) ==
    LET fresh == SelectSeq(q, LAMBDA x : Fresh(x, now))
        stale == CodesWhere(q, LAMBDA x : ~Fresh(x, now))
        full == Len(fresh) >= QueueLimit
    IN [queue |-> Append(IF full THEN Tail(fresh) ELSE fresh, e),
        ex |-> stale \cup (IF full THEN {Head(fresh).code} ELSE {})]

Held(c) == \/ c \in CodesOf(queue)
           \/ inflight.on /\ inflight.e.code = c
           \/ c \in CodesOf(pending)
           \/ \E j \in 1..Len(lucina) : lucina[j].pc = "got" /\ lucina[j].w.code = c

Init == /\ now = 0
        /\ born = [c \in Codes |-> None]
        /\ arrived = [c \in Codes |-> {}]
        /\ queue = <<>> /\ recent = {} /\ recentDisk = {} /\ inflight = NoFlight
        /\ window = IF Continuous THEN Cont ELSE Off
        /\ paused = FALSE /\ pending = <<>> /\ seen = {}
        /\ lucina = <<>>
        /\ drops = Drops /\ crashes = Crashes /\ restarts = Restarts /\ takes = Takes
        /\ stops = Stops /\ arms = Arms /\ tests = Tests /\ others = Others
        /\ delivered = <<>> /\ exempt = {} /\ preCrash = {} /\ leaked = FALSE
        /\ retryDedup = FALSE

(* SmsReceiver / OtpNotificationListener -> Bridge::forward. *)
Arrive(c, i) ==
    /\ ~inflight.on
    /\ i \notin arrived[c]
    /\ born[c] = None \/ now < born[c] + TTL
    /\ arrived' = [arrived EXCEPT ![c] = @ \cup {i}]
    /\ born' = IF born[c] = None THEN [born EXCEPT ![c] = now] ELSE born
    /\ LET r1 == {x \in recent : now - x[2] < TTL}
           e == [code |-> c, eid |-> <<c, i>>, recv |-> now, src |-> Copies[c][i]]
           r2 == r1 \cup {<<c, now>>}
           enq == Enqueue(queue, e)
       IN IF \E x \in r1 : x[1] = c
          THEN /\ recent' = r1                                \* Forward::Duplicate
               /\ UNCHANGED <<queue, recentDisk, inflight, exempt>>
          ELSE /\ recent' = r2
               /\ inflight' = [on |-> TRUE, e |-> e, kind |-> IF WriteAhead THEN "queued" ELSE "live",
                               stage |-> "req", out |-> "none"]
               /\ IF WriteAhead
                  THEN /\ recentDisk' = r2
                       /\ queue' = enq.queue
                       /\ exempt' = exempt \cup enq.ex
                  ELSE UNCHANGED <<recentDisk, queue, exempt>>
    /\ UNCHANGED <<now, relay, lucina, budget, delivered, preCrash, leaked, retryDedup>>

(* POST /incoming -> Store::receive *)
Serve ==
    /\ inflight.on /\ inflight.stage = "req"
    /\ LET r == Receive(inflight.e)
       IN /\ inflight' = [inflight EXCEPT !.stage = "resp", !.out = r.out]
          /\ pending' = r.pending
          /\ seen' = r.seen
          /\ exempt' = exempt \cup r.ex
          /\ retryDedup' = (retryDedup \/ r.out = "duplicate")
    /\ UNCHANGED <<env, queue, recent, recentDisk, window, paused, lucina, budget,
                   delivered, preCrash, leaked>>

(* Network error / timeout / unreadable 200 -> Outcome::Retry *)
LoseRequest ==
    /\ inflight.on /\ inflight.stage = "req" /\ drops > 0
    /\ drops' = drops - 1
    /\ inflight' = [inflight EXCEPT !.stage = "resp", !.out = "retry"]
    /\ UNCHANGED <<env, queue, recent, recentDisk, relay, lucina, crashes, restarts, takes,
                   stops, arms, tests, others, ghost>>
LoseResponse ==
    /\ inflight.on /\ inflight.stage = "resp" /\ inflight.out # "retry" /\ drops > 0
    /\ drops' = drops - 1
    /\ inflight' = [inflight EXCEPT !.out = "retry"]
    /\ UNCHANGED <<env, queue, recent, recentDisk, relay, lucina, crashes, restarts, takes,
                   stops, arms, tests, others, ghost>>

(* forward after send (enqueue, finish), or the flush_at loop body *)
PhoneGets ==
    /\ inflight.on /\ inflight.stage = "resp"
    /\ LET e == inflight.e
           o == inflight.out
           enq == Enqueue(queue, e)
           rest == SelectSeq(queue, LAMBDA x : x.eid # e.eid)
           dropped == IF o \in {"paused", "expired"} THEN {e.code} ELSE {}
       IN CASE inflight.kind = "live" ->
                 /\ queue' = IF o = "retry" THEN enq.queue ELSE queue
                 /\ exempt' = exempt \cup dropped \cup (IF o = "retry" THEN enq.ex ELSE {})
                 /\ recentDisk' = recent
            [] inflight.kind = "queued" ->
                 /\ queue' = IF o = "retry" THEN queue ELSE rest
                 /\ exempt' = exempt \cup dropped
                 /\ UNCHANGED recentDisk
            [] OTHER -> UNCHANGED <<queue, exempt, recentDisk>>
    /\ inflight' = NoFlight
    /\ UNCHANGED <<env, recent, relay, lucina, budget, delivered, preCrash, leaked, retryDedup>>

(* RetryJobService / BootReceiver -> Bridge::flush_at *)
Flush ==
    /\ ~inflight.on /\ queue # <<>>
    /\ LET fresh == SelectSeq(queue, LAMBDA x : Fresh(x, now))
       IN /\ queue' = fresh
          /\ exempt' = exempt \cup CodesWhere(queue, LAMBDA x : ~Fresh(x, now))
          /\ inflight' = IF fresh = <<>> THEN NoFlight
                         ELSE [on |-> TRUE, e |-> Head(fresh), kind |-> "queued",
                               stage |-> "req", out |-> "none"]
    /\ UNCHANGED <<env, recent, recentDisk, relay, lucina, budget, delivered, preCrash,
                   leaked, retryDedup>>

(* Bridge::probe (test: true) *)
SelfTest ==
    /\ ~inflight.on /\ tests > 0
    /\ tests' = tests - 1
    /\ inflight' = [on |-> TRUE, e |-> [code |-> TestCode, eid |-> <<TestCode, 0>>, recv |-> now,
                                        src |-> "sms"],
                    kind |-> "test", stage |-> "req", out |-> "none"]
    /\ UNCHANGED <<env, queue, recent, recentDisk, relay, lucina, drops, crashes, restarts,
                   takes, stops, arms, others, ghost>>

(* Reboot / process death: Bridge::open reloads state.bin and queue.bin. *)
Crash ==
    /\ crashes > 0 /\ (inflight.on \/ queue # <<>>)
    /\ crashes' = crashes - 1
    /\ inflight' = NoFlight
    /\ recent' = recentDisk
    /\ preCrash' = preCrash \cup {c \in Codes : born[c] # None}
    /\ UNCHANGED <<env, queue, recentDisk, relay, lucina, drops, restarts, takes, stops, arms,
                   tests, others, delivered, exempt, leaked, retryDedup>>

(* Time; expired dedup entries and seen ids never match again (dropped).  *)
Tick ==
    /\ now < MaxTime
    /\ now' = now + 1
    /\ recent' = {x \in recent : now' - x[2] < TTL}
    /\ recentDisk' = {x \in recentDisk : now' - x[2] < TTL}
    /\ seen' = {x \in seen : x[2] > now'}
    /\ UNCHANGED <<born, arrived, queue, inflight, window, paused, pending, lucina, budget, ghost>>

(* The owner's /otp: otp::run OtpAction::Take, one relay call per step. *)
OwnerAsks ==
    /\ takes > 0 /\ Len(lucina) < Threads
    /\ takes' = takes - 1
    /\ lucina' = Append(lucina, [pc |-> "take", w |-> NoWait])
    /\ UNCHANGED <<env, phone, relay, drops, crashes, restarts, stops, arms, tests, others, ghost>>

LucinaStep(i) ==
    LET l == lucina[i]
        rest == Remove(lucina, i)
        t == Take
        a == ArmR
    IN /\ i \in 1..Len(lucina)
       /\ CASE l.pc = "take" ->
                 /\ pending' = t.pending /\ window' = t.window
                 /\ exempt' = exempt \cup t.ex
                 /\ lucina' = Append(rest, IF t.has THEN [pc |-> "got", w |-> t.got]
                                           ELSE [pc |-> "status", w |-> NoWait])
                 /\ UNCHANGED <<paused, delivered>>
            [] l.pc = "got" ->
                 /\ IF LucinaExpiryCheck /\ l.w.exp <= now
                    THEN /\ exempt' = exempt \cup {l.w.code}          \* "expired code"
                         /\ UNCHANGED delivered
                    ELSE /\ delivered' = Append(delivered, [code |-> l.w.code, at |-> now,
                                                            src |-> l.w.src])
                         /\ UNCHANGED exempt
                 /\ lucina' = rest
                 /\ UNCHANGED <<pending, window, paused>>
            [] l.pc = "status" ->
                 /\ lucina' = IF Armed(window) THEN rest
                              ELSE Append(rest, [pc |-> "arm", w |-> NoWait])
                 /\ UNCHANGED <<pending, window, paused, exempt, delivered>>
            [] l.pc = "arm" ->
                 /\ pending' = a.pending /\ window' = a.window /\ paused' = a.paused
                 /\ exempt' = exempt \cup a.ex
                 /\ lucina' = rest
                 /\ UNCHANGED delivered
    /\ UNCHANGED <<env, phone, seen, budget, preCrash, leaked, retryDedup>>

(* /otp stop -> POST /disarm (paused file) *)
OwnerStop ==
    /\ stops > 0
    /\ stops' = stops - 1
    /\ window' = Off
    /\ pending' = <<>>
    /\ exempt' = exempt \cup CodesOf(pending)
    /\ paused' = IF Continuous THEN TRUE ELSE paused
    /\ UNCHANGED <<env, phone, seen, lucina, drops, crashes, restarts, takes, arms, tests, others,
                   delivered, preCrash, leaked, retryDedup>>

(* /otp arm -> POST /arm *)
OwnerArm ==
    /\ arms > 0
    /\ arms' = arms - 1
    /\ LET a == ArmR
       IN /\ pending' = a.pending /\ window' = a.window /\ paused' = a.paused
          /\ exempt' = exempt \cup a.ex
    /\ UNCHANGED <<env, phone, seen, lucina, drops, crashes, restarts, takes, stops, tests, others,
                   delivered, preCrash, leaked, retryDedup>>

(* Lucina MCP or a non-owner Telegram chat asks: reply::private_owner. *)
OtherAsks ==
    /\ others > 0
    /\ others' = others - 1
    /\ LET t == Take
       IN IF OwnerGuard
          THEN UNCHANGED <<pending, window, exempt, leaked>>
          ELSE /\ pending' = t.pending /\ window' = t.window
               /\ exempt' = exempt \cup t.ex
               /\ leaked' = (leaked \/ t.has)
    /\ UNCHANGED <<env, phone, paused, seen, lucina, drops, crashes, restarts, takes, stops,
                   arms, tests, delivered, preCrash, retryDedup>>

(* Relay::open after a restart: only the paused file survives. *)
RelayRestart ==
    /\ restarts > 0
    /\ restarts' = restarts - 1
    /\ pending' = <<>>
    /\ exempt' = exempt \cup CodesOf(pending)
    /\ seen' = IF PersistSeen THEN seen ELSE {}
    /\ window' = IF Continuous /\ ~paused THEN Cont ELSE Off
    /\ UNCHANGED <<env, phone, paused, lucina, drops, crashes, takes, stops, arms, tests, others,
                   delivered, preCrash, leaked, retryDedup>>

Next == \/ \E c \in Codes, i \in 1..2 : i <= Len(Copies[c]) /\ Arrive(c, i)
        \/ Serve \/ LoseRequest \/ LoseResponse \/ PhoneGets \/ Flush \/ SelfTest \/ Crash
        \/ Tick \/ OwnerAsks \/ \E i \in 1..Threads : LucinaStep(i)
        \/ OwnerStop \/ OwnerArm \/ OtherAsks \/ RelayRestart

Spec == Init /\ [][Next]_vars

(* ---- properties (names as in the Stateright model) ---------------------- *)
DeliveredCode(c) == \E j \in 1..Len(delivered) : delivered[j].code = c

AtMostOnce == \A c \in Codes : Cardinality({j \in 1..Len(delivered) : delivered[j].code = c}) <= 1
NeverAfterExpiry == \A j \in 1..Len(delivered) :
                        LET c == delivered[j].code
                        IN c \in Codes /\ born[c] # None /\ delivered[j].at < born[c] + TTL
OwnerOnly == ~leaked
TestNeverDelivered == /\ \A j \in 1..Len(delivered) : delivered[j].code # TestCode
                      /\ TestCode \notin CodesOf(pending)
(* Every received, unexpired, undelivered code that no explicit path        *)
(* dropped (409, 410, the phone's TTL, a pause or window end, a full queue, *)
(* a relay restart) is queued, in flight, waiting or in Lucina's hands.     *)
NoSilentLoss == \A c \in Codes : \/ born[c] = None
                                 \/ now >= born[c] + TTL
                                 \/ DeliveredCode(c)
                                 \/ c \in exempt
                                 \/ Held(c)
PendingBounded == Len(pending) <= RelayLimit /\ Len(queue) <= QueueLimit

(* Witnesses (cfgs that MUST FAIL): the interesting states are reachable. *)
NoRetryDedup == ~retryDedup
NoExpiry == ~\E c \in Codes : born[c] # None /\ now >= born[c] + TTL /\ ~DeliveredCode(c)
NoDeliveryAfterReboot == ~\E j \in 1..Len(delivered) : delivered[j].code \in preCrash
NoNotificationDelivery == ~\E j \in 1..Len(delivered) : delivered[j].src = "notification"
=============================================================================
