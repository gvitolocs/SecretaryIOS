------------------------------- MODULE OAuth -------------------------------
(***************************************************************************)
(* Lucina MCP OAuth 2.1 (crates/secretary-mcp/src/oauth.rs): authorization *)
(* code + PKCE S256, rotating refresh tokens, replay revokes the grant.    *)
(* TLA+ twin of verification/models/src/oauth.rs.                          *)
(*                                                                         *)
(* Token endpoint critical sections as in the code:                        *)
(*  code    (1) codes lock: redeemed? -> replay: revoke_grant              *)
(*          (2) PKCE etc. (a failed verifier burns the code)               *)
(*          (3) codes lock: set redeemed (already set -> "already used")   *)
(*          (4) issue_tokens: grant + tokens into oauth.json               *)
(*  refresh (1) file.read: used -> replay: revoke_grant                    *)
(*          (2) file.update: mark used if unused, else "already used";     *)
(*              then issue_tokens                                          *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS Budget,
          FixCodeReplay,    \* O1: mark redeemed + issue_tokens in one critical section; lost race = replay
          FixRefreshRace    \* O2: losing the rotate race revokes the grant

Who == {"client", "attacker"}

VARIABLES loginOk, consented, codeLive, codeHolders, redeemed, presented,
          grants, refresh, access, reqs, budget, rtReplays, stolen
vars == <<loginOk, consented, codeLive, codeHolders, redeemed, presented,
          grants, refresh, access, reqs, budget, rtReplays, stolen>>

\* grants: function grant id -> revoked;  refresh: set of [id, grant, used, holders]
Revoke(g) == /\ grants' = [x \in DOMAIN grants |-> IF x = g THEN TRUE ELSE grants[x]]
             /\ refresh' = {t \in refresh : t.grant # g}
             /\ access' = {a \in access : a[1] # g}

Issue(g, who, newGrant, gr, rf, ac, id) ==   \* <<grants, refresh, access>> after issue_tokens
    <<IF newGrant THEN [x \in DOMAIN gr \cup {g} |-> IF x = g THEN FALSE ELSE gr[x]] ELSE gr,
      rf \cup {[id |-> id, grant |-> g, used |-> FALSE, holders |-> {who}]},
      ac \cup {<<g, who>>}>>

Init == /\ loginOk = FALSE /\ consented = FALSE /\ codeLive = FALSE /\ codeHolders = {}
        /\ redeemed = 0 /\ presented = 0
        /\ grants = [x \in {} |-> FALSE] /\ refresh = {} /\ access = {}
        /\ reqs = <<>> /\ budget = Budget /\ rtReplays = 0 /\ stolen = FALSE

OwnerLogin(ok) == /\ ~loginOk /\ ~consented /\ loginOk' = ok
                  /\ UNCHANGED <<consented, codeLive, codeHolders, redeemed, presented,
                                 grants, refresh, access, reqs, budget, rtReplays, stolen>>
OwnerApprove == /\ loginOk /\ ~consented
                /\ consented' = TRUE /\ codeLive' = TRUE /\ codeHolders' = {"client"}
                /\ UNCHANGED <<loginOk, redeemed, presented, grants, refresh, access,
                               reqs, budget, rtReplays, stolen>>
CodeLeaks == /\ codeLive /\ "attacker" \notin codeHolders /\ codeHolders' = codeHolders \cup {"attacker"}
             /\ UNCHANGED <<loginOk, consented, codeLive, redeemed, presented, grants,
                            refresh, access, reqs, budget, rtReplays, stolen>>
RefreshLeaks == /\ \E t \in refresh : "attacker" \notin t.holders
                     /\ refresh' = (refresh \ {t}) \cup {[t EXCEPT !.holders = @ \cup {"attacker"}]}
                /\ UNCHANGED <<loginOk, consented, codeLive, codeHolders, redeemed, presented,
                               grants, access, reqs, budget, rtReplays, stolen>>

Redeem(who) == /\ budget > 0 /\ Len(reqs) < 2 /\ who \in codeHolders
               /\ reqs' = Append(reqs, [who |-> who, kind |-> "code", step |-> 1, grant |-> 0, rt |-> 0, n |-> budget])
               /\ budget' = budget - 1
               /\ UNCHANGED <<loginOk, consented, codeLive, codeHolders, redeemed, presented,
                              grants, refresh, access, rtReplays, stolen>>
UseRefresh(who) == /\ budget > 0 /\ Len(reqs) < 2
                   /\ \E t \in refresh : who \in t.holders
                        /\ reqs' = Append(reqs, [who |-> who, kind |-> "refresh", step |-> 1, grant |-> t.grant, rt |-> t.id, n |-> budget])
                   /\ budget' = budget - 1
                   /\ UNCHANGED <<loginOk, consented, codeLive, codeHolders, redeemed, presented,
                                  grants, refresh, access, rtReplays, stolen>>

Done(i) == reqs' = [j \in 1..(Len(reqs) - 1) |-> IF j < i THEN reqs[j] ELSE reqs[j + 1]]
Advance(i, r) == reqs' = [reqs EXCEPT ![i] = r]

CodeStep(i) ==
    LET r == reqs[i] IN
    /\ r.kind = "code"
    /\ CASE r.step = 1 ->
              /\ presented' = presented + 1
              /\ IF redeemed # 0
                 THEN /\ (IF redeemed \in DOMAIN grants THEN Revoke(redeemed) ELSE UNCHANGED <<grants, refresh, access>>)
                      /\ Done(i) /\ UNCHANGED <<codeLive, codeHolders>>
                 ELSE IF r.who = "attacker"    \* no verifier: PKCE fails, the code is burned
                      THEN /\ codeLive' = FALSE /\ codeHolders' = {} /\ Done(i)
                           /\ UNCHANGED <<grants, refresh, access>>
                      ELSE /\ Advance(i, [r EXCEPT !.step = 2])
                           /\ UNCHANGED <<codeLive, codeHolders, grants, refresh, access>>
              /\ UNCHANGED <<redeemed, stolen>>
         [] r.step = 2 ->
              /\ IF ~codeLive THEN /\ Done(i) /\ UNCHANGED <<redeemed, grants, refresh, access, stolen>>
                 ELSE IF redeemed = 0
                      THEN /\ redeemed' = 1
                           /\ IF FixCodeReplay   \* issue_tokens inside the same critical section
                              THEN LET is == Issue(1, r.who, TRUE, grants, refresh, access, r.n)
                                   IN /\ grants' = is[1] /\ refresh' = is[2] /\ access' = is[3]
                                      /\ stolen' = (stolen \/ r.who = "attacker") /\ Done(i)
                              ELSE /\ Advance(i, [r EXCEPT !.step = 3, !.grant = 1])
                                   /\ UNCHANGED <<grants, refresh, access, stolen>>
                      ELSE /\ Done(i) /\ UNCHANGED <<redeemed, stolen>>   \* "code already used"
                           /\ IF FixCodeReplay /\ redeemed \in DOMAIN grants THEN Revoke(redeemed)
                              ELSE UNCHANGED <<grants, refresh, access>>
              /\ UNCHANGED <<presented, codeLive, codeHolders>>
         [] r.step = 3 ->
              /\ LET is == Issue(r.grant, r.who, TRUE, grants, refresh, access, r.n)
                 IN grants' = is[1] /\ refresh' = is[2] /\ access' = is[3]
              /\ stolen' = (stolen \/ r.who = "attacker")
              /\ Done(i)
              /\ UNCHANGED <<presented, redeemed, codeLive, codeHolders>>
    /\ UNCHANGED <<loginOk, consented, budget, rtReplays>>

RefreshStep(i) ==
    LET r == reqs[i]
        tok == {t \in refresh : t.id = r.rt}
    IN
    /\ r.kind = "refresh"
    /\ CASE r.step = 1 ->
              IF tok = {} \/ (r.grant \in DOMAIN grants /\ grants[r.grant])
              THEN /\ Done(i) /\ UNCHANGED <<grants, refresh, access, rtReplays>>
              ELSE IF (CHOOSE t \in tok : TRUE).used
                   THEN /\ rtReplays' = rtReplays + 1 /\ Revoke(r.grant) /\ Done(i)
                   ELSE /\ Advance(i, [r EXCEPT !.step = 2]) /\ UNCHANGED <<grants, refresh, access, rtReplays>>
         [] r.step = 2 ->
              IF tok = {} THEN /\ Done(i) /\ UNCHANGED <<grants, refresh, access, rtReplays>>
              ELSE LET t == CHOOSE t \in tok : TRUE IN
                   IF ~t.used
                   THEN LET rf == (refresh \ {t}) \cup {[t EXCEPT !.used = TRUE]}
                            is == Issue(r.grant, r.who, FALSE, grants, rf, access, r.n)
                        IN /\ grants' = is[1] /\ refresh' = is[2] /\ access' = is[3]
                           /\ Done(i) /\ UNCHANGED rtReplays
                   ELSE /\ rtReplays' = rtReplays + 1 /\ Done(i)
                        /\ IF FixRefreshRace THEN Revoke(r.grant) ELSE UNCHANGED <<grants, refresh, access>>
    /\ UNCHANGED <<loginOk, consented, codeLive, codeHolders, redeemed, presented, budget, stolen>>

Next == \/ OwnerLogin(TRUE) \/ OwnerLogin(FALSE) \/ OwnerApprove \/ CodeLeaks \/ RefreshLeaks
        \/ \E w \in Who : Redeem(w) \/ UseRefresh(w)
        \/ \E i \in 1..Len(reqs) : CodeStep(i) \/ RefreshStep(i)
Spec == Init /\ [][Next]_vars

AllDone == reqs = <<>>
CodeUsedOnce == Cardinality(DOMAIN grants) <= 1
NoTokenWithoutLoginAndConsent == DOMAIN grants # {} => loginOk /\ consented
VerifierBound == ~stolen
CodeReplayRevokesGrant == (AllDone /\ presented >= 2 /\ redeemed \in DOMAIN grants) => grants[redeemed]
RefreshReuseRevokesGrant == (AllDone /\ rtReplays > 0) => \A g \in DOMAIN grants : grants[g]
NeverRefreshed == ~\E t \in refresh : t.used
=============================================================================
