-------------------------------- MODULE Share --------------------------------
(***************************************************************************)
(* Share to Lucina: POST /v1/share (lucina-server/src/share/mod.rs, branch *)
(* lucina-rs; iOS ShareShared, ShareUploads, ChatShares, Timeline.merge on *)
(* share-ios; web state/chat.ts). TLA+ twin of                              *)
(* verification/models/src/share.rs, with the same state, so TLC's        *)
(* distinct-state count equals Stateright's for the same bounds.           *)
(*                                                                         *)
(* Clients: 0 = the chat composer (share 0, an album of AlbumFiles files,  *)
(* background upload, Retry with the same shareId); 1 = a share extension  *)
(* (share 1), "bg" with an App Group (background upload: survives a kill,  *)
(* the app retries the same body) or "fg" without one (a kill loses the    *)
(* body; the owner shares again: share 2). The attacker (client 9, share   *)
(* 3) has no owner auth: through p1 forward_auth answers 401, straight at  *)
(* lucina-rs authenticate answers 401 (no X-Share-Gateway).                *)
(*                                                                         *)
(* receive, under the ingest mutex: (1) lookup status.json (found -> 202   *)
(* duplicate), (2) move + ingest the files, (3) write status.json, (4)     *)
(* ensure_user + spawn + 202. A client disconnect drops the handler at an  *)
(* await of steps 1-2. One worker runs process: ensure_user,               *)
(* existing_reply (the newest Window items; 0 = all), read each part, one  *)
(* turn writes one Lucina item, status done. recover respawns every        *)
(* processing status after a restart (a request may take the mutex first). *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS ExtKinds,         \* subset of {"bg", "fg"}: one initial state each ({} = composer only)
          AlbumFiles,       \* files in the composer's share
          MaxSends,         \* requests per shareId (1 + retries)
          Crashes, NetFaults, AppendFaults, ProcFaults, Others, Reshares, Attacks,
          Window,           \* existing_reply scans the newest Window items; 0 = every item (fix)
          ReconcileByShareId, \* the chat view matches the server item by meta.shareId too
          KeyCheck,         \* lucina-rs checks X-Share-Gateway (the attacker may reach it directly)
          FixSpawn          \* receive spawns process even when ensure_user failed

Shares == 0..3
OwnerShares == {0, 1, 2}
Attacker == 9
NoReq == [client |-> Attacker, share |-> 0, owner |-> FALSE, direct |-> FALSE]
NoProc == [share |-> 0, read |-> 0, written |-> "idle"]
Composer == [kind |-> "composer", share |-> 0, phase |-> "ready", killed |-> FALSE]

VARIABLES clients, net, rxStep, rxReq, status, media, timeline, work, proc, up, recovered,
          view, sent, crashes, netFaults, appendFaults, procFaults, others, reshares, attacks,
          dupHit, revived, attackRejected
vars == <<clients, net, rxStep, rxReq, status, media, timeline, work, proc, up, recovered,
          view, sent, crashes, netFaults, appendFaults, procFaults, others, reshares, attacks,
          dupHit, revived, attackRejected>>
budgets == <<crashes, netFaults, appendFaults, procFaults, others, reshares, attacks>>

Files(x) == IF x = 0 THEN AlbumFiles ELSE 1
Range(t) == {t[i] : i \in DOMAIN t}
Count(t, e) == Cardinality({i \in DOMAIN t : t[i] = e})
EnsureUser(t, x) == IF <<"user", x>> \in Range(t) THEN t ELSE Append(t, <<"user", x>>)
ExistingReply(t, x) ==
    LET n == IF Window = 0 \/ Window > Len(t) THEN Len(t) ELSE Window
    IN \E i \in (Len(t) - n + 1)..Len(t) : t[i] = <<"lucina", x>>

ClientsInit ==
    IF ExtKinds = {} THEN {[i \in {0} |-> Composer]}
    ELSE {[i \in {0, 1} |-> IF i = 0 THEN Composer
                            ELSE [kind |-> k, share |-> 1, phase |-> "ready", killed |-> FALSE]] : k \in ExtKinds}

Init == /\ clients \in ClientsInit
        /\ net = {} /\ rxStep = "none" /\ rxReq = NoReq
        /\ status = [x \in Shares |-> "none"] /\ media = {} /\ timeline = <<>>
        /\ work = <<>> /\ proc = NoProc /\ up = TRUE /\ recovered = TRUE
        /\ view = [local |-> FALSE, server |-> FALSE, delivered |-> FALSE]
        /\ sent = [x \in Shares |-> 0]
        /\ crashes = Crashes /\ netFaults = NetFaults /\ appendFaults = AppendFaults
        /\ procFaults = ProcFaults /\ others = Others /\ reshares = Reshares /\ attacks = Attacks
        /\ dupHit = FALSE /\ revived = {} /\ attackRejected = FALSE

\* A response reaches the client if it is still waiting (the attacker and a
\* killed foreground extension do not listen). A 202 for the composer runs
\* ShareReconcile.accept: the local bubble goes, the server item stays.
RespondTo(c, ok, dup) ==
    IF c \in DOMAIN clients /\ clients[c].phase = "inflight"
    THEN /\ clients' = [clients EXCEPT ![c].phase = IF ok THEN "accepted" ELSE "failed"]
         /\ dupHit' = (dupHit \/ (ok /\ dup))
         /\ view' = IF ok /\ clients[c].kind = "composer"
                    THEN [view EXCEPT !.local = FALSE, !.server = TRUE] ELSE view
    ELSE UNCHANGED <<clients, dupHit, view>>

Req(i, x) == [client |-> i, share |-> x, owner |-> TRUE, direct |-> FALSE]

\* ---- clients -------------------------------------------------------------
Send(i) ==
    LET c == clients[i] IN
    /\ \/ c.phase = "ready"
       \/ c.phase = "failed" /\ c.kind # "fg" /\ sent[c.share] < MaxSends
    /\ clients' = [clients EXCEPT ![i].phase = "inflight"]
    /\ view' = IF c.kind = "composer" /\ c.phase = "ready" THEN [view EXCEPT !.local = TRUE] ELSE view
    /\ sent' = [sent EXCEPT ![c.share] = @ + 1]
    /\ net' = net \cup {Req(i, c.share)}
    /\ UNCHANGED <<rxStep, rxReq, status, media, timeline, work, proc, up, recovered, budgets,
                   dupHit, revived, attackRejected>>

Reshare(i) ==
    LET c == clients[i] IN
    /\ c.kind = "fg" /\ c.phase \in {"failed", "lost"} /\ reshares > 0 /\ c.share # 2
    /\ reshares' = reshares - 1
    /\ clients' = [clients EXCEPT ![i].share = 2, ![i].phase = "inflight"]
    /\ sent' = [sent EXCEPT ![2] = @ + 1]
    /\ net' = net \cup {Req(i, 2)}
    /\ UNCHANGED <<rxStep, rxReq, status, media, timeline, work, proc, up, recovered, view,
                   crashes, netFaults, appendFaults, procFaults, others, attacks,
                   dupHit, revived, attackRejected>>

Kill(i) ==
    LET c == clients[i] IN
    /\ c.kind # "composer" /\ c.phase = "inflight" /\ ~c.killed
    /\ IF c.kind = "fg"
       THEN /\ clients' = [clients EXCEPT ![i].killed = TRUE, ![i].phase = "lost"]
            /\ net' = {r \in net : r.client # i}
            \* Caddy cancels the upstream request: dropped at an await before status.json.
            /\ IF rxStep \in {"new", "ingested"} /\ rxReq.client = i
               THEN rxStep' = "none" /\ rxReq' = NoReq
               ELSE UNCHANGED <<rxStep, rxReq>>
       ELSE /\ clients' = [clients EXCEPT ![i].killed = TRUE]
            /\ UNCHANGED <<net, rxStep, rxReq>>
    /\ UNCHANGED <<status, media, timeline, work, proc, up, recovered, view, sent, budgets,
                   dupHit, revived, attackRejected>>

Attack(direct) ==
    /\ attacks > 0
    /\ attacks' = attacks - 1
    /\ net' = net \cup {[client |-> Attacker, share |-> 3, owner |-> FALSE, direct |-> direct]}
    /\ UNCHANGED <<clients, rxStep, rxReq, status, media, timeline, work, proc, up, recovered,
                   view, sent, crashes, netFaults, appendFaults, procFaults, others, reshares,
                   dupHit, revived, attackRejected>>

DropRequest(r) ==
    /\ netFaults > 0 /\ r \in net /\ r.owner
    /\ netFaults' = netFaults - 1
    /\ net' = net \ {r}
    /\ RespondTo(r.client, FALSE, FALSE)
    /\ UNCHANGED <<rxStep, rxReq, status, media, timeline, work, proc, up, recovered, sent,
                   crashes, appendFaults, procFaults, others, reshares, attacks,
                   revived, attackRejected>>

\* ---- receive ---------------------------------------------------------------
Deliver(r) ==
    /\ r \in net /\ rxStep = "none"
    /\ net' = net \ {r}
    /\ IF ~r.owner /\ (~r.direct \/ KeyCheck)
       THEN /\ attackRejected' = TRUE      \* forward_auth 401, or authenticate 401
            /\ UNCHANGED <<clients, dupHit, view, rxStep, rxReq>>
       ELSE /\ UNCHANGED attackRejected
            /\ IF ~up THEN RespondTo(r.client, FALSE, FALSE) /\ UNCHANGED <<rxStep, rxReq>>
               ELSE IF status[r.share] # "none"
                    THEN RespondTo(r.client, TRUE, TRUE) /\ UNCHANGED <<rxStep, rxReq>>
                    ELSE /\ rxStep' = "new" /\ rxReq' = r
                         /\ UNCHANGED <<clients, dupHit, view>>
    /\ UNCHANGED <<status, media, timeline, work, proc, up, recovered, sent, budgets, revived>>

RxIngest ==
    /\ rxStep = "new"
    /\ media' = media \cup {rxReq.share} /\ rxStep' = "ingested"
    /\ UNCHANGED <<clients, net, rxReq, status, timeline, work, proc, up, recovered, view, sent,
                   budgets, dupHit, revived, attackRejected>>

RxSave ==
    /\ rxStep = "ingested"
    /\ status' = [status EXCEPT ![rxReq.share] = "processing"] /\ rxStep' = "saved"
    /\ UNCHANGED <<clients, net, rxReq, media, timeline, work, proc, up, recovered, view, sent,
                   budgets, dupHit, revived, attackRejected>>

\* ensure_user, spawn, 202 (ok = FALSE: the 202 is lost on the way back)
RxFinish(ok) ==
    /\ rxStep = "saved" /\ (ok \/ netFaults > 0)
    /\ timeline' = EnsureUser(timeline, rxReq.share)
    /\ work' = Append(work, rxReq.share)
    /\ netFaults' = IF ok THEN netFaults ELSE netFaults - 1
    /\ RespondTo(rxReq.client, ok, FALSE)
    /\ rxStep' = "none" /\ rxReq' = NoReq
    /\ UNCHANGED <<net, status, media, proc, up, recovered, sent, crashes, appendFaults,
                   procFaults, others, reshares, attacks, revived, attackRejected>>

\* ensure_user fails (timeline I/O): 500; today without spawning process.
RxAppendFails ==
    /\ rxStep = "saved" /\ appendFaults > 0
    /\ appendFaults' = appendFaults - 1
    /\ work' = IF FixSpawn THEN Append(work, rxReq.share) ELSE work
    /\ RespondTo(rxReq.client, FALSE, FALSE)
    /\ rxStep' = "none" /\ rxReq' = NoReq
    /\ UNCHANGED <<net, status, media, timeline, proc, up, recovered, sent, crashes, netFaults,
                   procFaults, others, reshares, attacks, revived, attackRejected>>

\* ---- process (one worker) -------------------------------------------------
ProcBegin ==
    /\ up /\ proc = NoProc /\ work # <<>>
    /\ LET x == Head(work)
           t == EnsureUser(timeline, x)
       IN /\ work' = Tail(work) /\ timeline' = t
          /\ IF ExistingReply(t, x)
             THEN status' = [status EXCEPT ![x] = "done"] /\ UNCHANGED proc
             ELSE proc' = [share |-> x, read |-> 0, written |-> "none"] /\ UNCHANGED status
    /\ UNCHANGED <<clients, net, rxStep, rxReq, media, up, recovered, view, sent, budgets,
                   dupHit, revived, attackRejected>>

ProcRead ==
    /\ up /\ proc # NoProc /\ proc.written = "none" /\ proc.read < Files(proc.share)
    /\ proc' = [proc EXCEPT !.read = @ + 1]
    /\ UNCHANGED <<clients, net, rxStep, rxReq, status, media, timeline, work, up, recovered,
                   view, sent, budgets, dupHit, revived, attackRejected>>

\* The Lucina turn (written = "done") or an error before it (written = "error").
ProcWrite(w) ==
    /\ up /\ proc # NoProc /\ proc.written = "none"
    /\ IF w = "done" THEN proc.read = Files(proc.share) /\ UNCHANGED procFaults
       ELSE procFaults > 0 /\ procFaults' = procFaults - 1
    /\ timeline' = Append(timeline, <<"lucina", proc.share>>)
    /\ proc' = [proc EXCEPT !.written = w]
    /\ UNCHANGED <<clients, net, rxStep, rxReq, status, media, work, up, recovered, view, sent,
                   crashes, netFaults, appendFaults, others, reshares, attacks,
                   dupHit, revived, attackRejected>>

ProcSave ==
    /\ up /\ proc.written \in {"done", "error"}
    /\ status' = [status EXCEPT ![proc.share] = proc.written] /\ proc' = NoProc
    /\ UNCHANGED <<clients, net, rxStep, rxReq, media, timeline, work, up, recovered, view, sent,
                   budgets, dupHit, revived, attackRejected>>

\* ---- environment -------------------------------------------------------------
OtherAppend ==
    /\ others > 0
    /\ others' = others - 1 /\ timeline' = Append(timeline, <<"other", 0>>)
    /\ UNCHANGED <<clients, net, rxStep, rxReq, status, media, work, proc, up, recovered, view,
                   sent, crashes, netFaults, appendFaults, procFaults, reshares, attacks,
                   dupHit, revived, attackRejected>>

Crash ==
    /\ up /\ crashes > 0
    /\ crashes' = crashes - 1
    /\ IF rxStep # "none" THEN RespondTo(rxReq.client, FALSE, FALSE)
       ELSE UNCHANGED <<clients, dupHit, view>>
    /\ rxStep' = "none" /\ rxReq' = NoReq /\ work' = <<>> /\ proc' = NoProc
    /\ up' = FALSE /\ recovered' = FALSE
    /\ UNCHANGED <<net, status, media, timeline, sent, netFaults, appendFaults, procFaults,
                   others, reshares, attacks, revived, attackRejected>>

Restart ==
    /\ ~up /\ up' = TRUE
    /\ UNCHANGED <<clients, net, rxStep, rxReq, status, media, timeline, work, proc, recovered,
                   view, sent, budgets, dupHit, revived, attackRejected>>

Recover ==
    /\ up /\ ~recovered /\ rxStep = "none"
    /\ recovered' = TRUE
    /\ work' = work \o SelectSeq(<<0, 1, 2, 3>>, LAMBDA x : status[x] = "processing")
    /\ revived' = revived \cup {x \in Shares : status[x] = "processing"}
    /\ UNCHANGED <<clients, net, rxStep, rxReq, status, media, timeline, proc, up, view, sent,
                   budgets, dupHit, attackRejected>>

\* The chat's timeline stream delivers the composer share's user item.
TimelineDeliver ==
    /\ ~view.delivered /\ <<"user", 0>> \in Range(timeline)
    /\ view' = [local |-> IF ReconcileByShareId THEN FALSE ELSE view.local,
                server |-> TRUE, delivered |-> TRUE]
    /\ UNCHANGED <<clients, net, rxStep, rxReq, status, media, timeline, work, proc, up,
                   recovered, sent, budgets, dupHit, revived, attackRejected>>

Next == \/ \E i \in DOMAIN clients : Send(i) \/ Reshare(i) \/ Kill(i)
        \/ \E d \in BOOLEAN : Attack(d)
        \/ \E r \in net : Deliver(r) \/ DropRequest(r)
        \/ RxIngest \/ RxSave \/ RxFinish(TRUE) \/ RxFinish(FALSE) \/ RxAppendFails
        \/ ProcBegin \/ ProcRead \/ ProcWrite("done") \/ ProcWrite("error") \/ ProcSave
        \/ OtherAppend \/ Crash \/ Restart \/ Recover \/ TimelineDeliver
Spec == Init /\ [][Next]_vars

\* ---- invariants (Stateright names in brackets) --------------------------------
Done(x) == status[x] \in {"done", "error"}
\* SH1 [sh1_one_user_item]
OneUserItem == \A x \in OwnerShares :
    LET n == Count(timeline, <<"user", x>>) IN n <= 1 /\ (Done(x) => n = 1)
\* SH2 [sh2_answered_once]
AnsweredOnce == \A x \in OwnerShares :
    LET n == Count(timeline, <<"lucina", x>>) IN n <= 1 /\ (Done(x) => n = 1)
\* SH3 [sh3_retry_idempotent]
RetryIdempotent == \A x \in OwnerShares :
    sent[x] >= 2 => Count(timeline, <<"user", x>>) <= 1 /\ Count(timeline, <<"lucina", x>>) <= 1
\* SH4 [sh4_owner_only]
OwnerOnly == /\ 3 \notin media /\ status[3] = "none"
             /\ Count(timeline, <<"user", 3>>) = 0 /\ Count(timeline, <<"lucina", 3>>) = 0
\* SH5 [sh5_one_bubble]
OneBubble == ~(view.local /\ view.server)
\* SH6 [sh6_accepted_completes]
Quiet == up /\ recovered /\ rxStep = "none" /\ work = <<>> /\ proc = NoProc /\ net = {}
AcceptedCompletes == Quiet => \A x \in Shares : status[x] # "processing"

\* ---- witnesses (MUST FAIL cfgs: the interesting state is reached) -------------
NoRetryDuplicate == ~dupHit
NoBackgroundKillDelivered ==
    ~\E i \in DOMAIN clients : clients[i].kind = "bg" /\ clients[i].killed /\ clients[i].phase = "accepted"
NoForegroundKillLost ==
    ~\E i \in DOMAIN clients : /\ clients[i].kind = "fg" /\ clients[i].phase = "lost"
                               /\ status[clients[i].share] = "none"
                               /\ (rxStep = "none" \/ rxReq.share # clients[i].share)
NoCrashRecoveryCompleted == ~\E x \in revived : status[x] = "done"
NoAlbumAnsweredOnce == ~(AlbumFiles >= 2 /\ status[0] = "done"
                         /\ Count(timeline, <<"user", 0>>) = 1 /\ Count(timeline, <<"lucina", 0>>) = 1)
NoUnauthenticatedRefused == ~attackRejected
=============================================================================
