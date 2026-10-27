------------------------------- MODULE Deploy -------------------------------
(***************************************************************************)
(* Multi-session merge and deploy: /tmp/lucina-merge.lock, the shared main *)
(* checkout (with uncommitted live work), and deploys of secretary-server, *)
(* the web app and lucina-rs. TLA+ twin of                                 *)
(* verification/models/src/deploy.rs (same actions, switches, invariants). *)
(*                                                                         *)
(* Commit content is a set of features, so "A is an ancestor of B" is      *)
(* "A \subseteq B". Session i's feature is <<"f", i>>; the uncommitted     *)
(* work it leaves in the shared checkout is <<"w", i>>.                    *)
(*                                                                         *)
(* 25 Oct 2026: secretary-server had been deployed from the dirty checkout *)
(* (calls + hermes, uncommitted); Lucina Ink then deployed from a clean    *)
(* main without calls and /v1/calls/* went from 401 to 404.                *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS N,              \* sessions
          Runtimes,       \* secretary-server, web, ... (count)
          LockRequired,   \* merges, builds and deploys only inside the lock
          CheckoutBuilds, \* a deploy may be built from the shared checkout
          BranchBuilds,   \* a deploy may be built from a feature branch
          Guard           \* deploy-guard check before every push

Sess == 1..N
RT == 1..Runtimes
F(i) == <<"f", i>>
W(i) == <<"w", i>>
AllF == {F(i) : i \in Sess}

VARIABLES main, dirty, lock, prodC, prodDirty, ever, dirtyDeployed, refused,
          hasBranch, branchC, hasRead, readC, merged,
          hasBuild, buildC, buildDirty, target, locked, edited, deployed
vars == <<main, dirty, lock, prodC, prodDirty, ever, dirtyDeployed, refused,
          hasBranch, branchC, hasRead, readC, merged,
          hasBuild, buildC, buildDirty, target, locked, edited, deployed>>

Init == /\ main = {} /\ dirty = {} /\ lock = 0
        /\ prodC = [r \in RT |-> {}] /\ prodDirty = [r \in RT |-> FALSE]
        /\ ever = [r \in RT |-> {}] /\ dirtyDeployed = FALSE /\ refused = FALSE
        /\ hasBranch = [i \in Sess |-> FALSE] /\ branchC = [i \in Sess |-> {}]
        /\ hasRead = [i \in Sess |-> FALSE] /\ readC = [i \in Sess |-> {}]
        /\ merged = [i \in Sess |-> FALSE]
        /\ hasBuild = [i \in Sess |-> FALSE] /\ buildC = [i \in Sess |-> {}]
        /\ buildDirty = [i \in Sess |-> FALSE] /\ target = [i \in Sess |-> 0]
        /\ locked = [i \in Sess |-> FALSE] /\ edited = [i \in Sess |-> FALSE]
        /\ deployed = [i \in Sess |-> {}]

Done(i) == deployed[i] = RT
Work(i) == (locked[i] \/ ~LockRequired) /\ ~Done(i)
InSection(i) == hasRead[i] \/ hasBuild[i] \/ target[i] # 0

\* deploy-guard check: clean tree, commit on main, live build an ancestor of the commit.
GuardAllows(r, c, d) == ~d /\ c \subseteq main /\ prodC[r] \subseteq c

Branch(i) == /\ ~hasBranch[i]
             /\ hasBranch' = [hasBranch EXCEPT ![i] = TRUE]
             /\ branchC' = [branchC EXCEPT ![i] = main \cup {F(i)}]
             /\ UNCHANGED <<main, dirty, lock, prodC, prodDirty, ever, dirtyDeployed, refused,
                            hasRead, readC, merged, hasBuild, buildC, buildDirty, target,
                            locked, edited, deployed>>

EditCheckout(i) == /\ ~edited[i] /\ ~hasBranch[i]
                   /\ dirty' = dirty \cup {W(i)}
                   /\ edited' = [edited EXCEPT ![i] = TRUE]
                   /\ UNCHANGED <<main, lock, prodC, prodDirty, ever, dirtyDeployed, refused,
                                  hasBranch, branchC, hasRead, readC, merged, hasBuild, buildC,
                                  buildDirty, target, locked, deployed>>

Acquire(i) == /\ hasBranch[i] /\ ~locked[i] /\ lock = 0 /\ ~Done(i)
              /\ lock' = i /\ locked' = [locked EXCEPT ![i] = TRUE]
              /\ UNCHANGED <<main, dirty, prodC, prodDirty, ever, dirtyDeployed, refused,
                             hasBranch, branchC, hasRead, readC, merged, hasBuild, buildC,
                             buildDirty, target, edited, deployed>>

MergeRead(i) == /\ Work(i) /\ hasBranch[i] /\ ~merged[i] /\ ~hasRead[i]
                /\ hasRead' = [hasRead EXCEPT ![i] = TRUE]
                /\ readC' = [readC EXCEPT ![i] = main]
                /\ UNCHANGED <<main, dirty, lock, prodC, prodDirty, ever, dirtyDeployed, refused,
                               hasBranch, branchC, merged, hasBuild, buildC, buildDirty, target,
                               locked, edited, deployed>>

MergeWrite(i) == /\ hasRead[i]
                 /\ main' = readC[i] \cup branchC[i]
                 /\ hasRead' = [hasRead EXCEPT ![i] = FALSE]
                 /\ merged' = [merged EXCEPT ![i] = TRUE]
                 /\ UNCHANGED <<dirty, lock, prodC, prodDirty, ever, dirtyDeployed, refused,
                                hasBranch, branchC, readC, hasBuild, buildC, buildDirty, target,
                                locked, edited, deployed>>

CommitWip(i) == /\ Work(i) /\ dirty # {} /\ ~hasBuild[i]
                /\ main' = main \cup dirty /\ dirty' = {}
                /\ UNCHANGED <<lock, prodC, prodDirty, ever, dirtyDeployed, refused,
                               hasBranch, branchC, hasRead, readC, merged, hasBuild, buildC,
                               buildDirty, target, locked, edited, deployed>>

CanBuild(i) == Work(i) /\ ~hasBuild[i] /\ ~hasRead[i]

SetBuild(i, c, d) == /\ hasBuild' = [hasBuild EXCEPT ![i] = TRUE]
                     /\ buildC' = [buildC EXCEPT ![i] = c]
                     /\ buildDirty' = [buildDirty EXCEPT ![i] = d]
                     /\ UNCHANGED <<main, dirty, lock, prodC, prodDirty, ever, dirtyDeployed,
                                    refused, hasBranch, branchC, hasRead, readC, merged, target,
                                    locked, edited, deployed>>

BuildMain(i) == CanBuild(i) /\ merged[i] /\ SetBuild(i, main, FALSE)
BuildCheckout(i) == CheckoutBuilds /\ CanBuild(i) /\ merged[i] /\ SetBuild(i, main \cup dirty, dirty # {})
BuildBranch(i) == BranchBuilds /\ CanBuild(i) /\ hasBranch[i] /\ SetBuild(i, branchC[i], FALSE)

Check(i, r) == /\ hasBuild[i] /\ target[i] = 0 /\ r \notin deployed[i]
               /\ IF Guard /\ ~GuardAllows(r, buildC[i], buildDirty[i])
                  THEN /\ refused' = TRUE
                       /\ hasBuild' = [hasBuild EXCEPT ![i] = FALSE]
                       /\ UNCHANGED target
                  ELSE /\ target' = [target EXCEPT ![i] = r]
                       /\ UNCHANGED <<refused, hasBuild>>
               /\ UNCHANGED <<main, dirty, lock, prodC, prodDirty, ever, dirtyDeployed,
                              hasBranch, branchC, hasRead, readC, merged, buildC, buildDirty,
                              locked, edited, deployed>>

Push(i) == /\ target[i] # 0
           /\ LET r == target[i] IN
                /\ prodC' = [prodC EXCEPT ![r] = buildC[i]]
                /\ prodDirty' = [prodDirty EXCEPT ![r] = buildDirty[i]]
                /\ ever' = [ever EXCEPT ![r] = @ \cup buildC[i]]
                /\ deployed' = [deployed EXCEPT ![i] = @ \cup {r}]
           /\ dirtyDeployed' = (dirtyDeployed \/ buildDirty[i])
           /\ target' = [target EXCEPT ![i] = 0]
           /\ hasBuild' = [hasBuild EXCEPT ![i] = FALSE]
           /\ UNCHANGED <<main, dirty, lock, refused, hasBranch, branchC, hasRead, readC,
                          merged, buildC, buildDirty, locked, edited>>

Release(i) == /\ locked[i] /\ ~hasRead[i] /\ ~hasBuild[i]
              /\ lock' = 0 /\ locked' = [locked EXCEPT ![i] = FALSE]
              /\ UNCHANGED <<main, dirty, prodC, prodDirty, ever, dirtyDeployed, refused,
                             hasBranch, branchC, hasRead, readC, merged, hasBuild, buildC,
                             buildDirty, target, edited, deployed>>

Next == \E i \in Sess :
          \/ Branch(i) \/ EditCheckout(i) \/ Acquire(i) \/ MergeRead(i) \/ MergeWrite(i)
          \/ CommitWip(i) \/ BuildMain(i) \/ BuildCheckout(i) \/ BuildBranch(i)
          \/ (\E r \in RT : Check(i, r)) \/ Push(i) \/ Release(i)

Spec == Init /\ [][Next]_vars

\* ---- invariants (Stateright names in the comments) ----
NoRollback == \A r \in RT : ever[r] \subseteq prodC[r]                  \* no_rollback
ProdOnMain == \A r \in RT : ~prodDirty[r] /\ prodC[r] \subseteq main     \* prod_on_main
SingleHolder == Cardinality({i \in Sess : InSection(i)}) <= 1           \* single_holder
NoDirtyDeploy == ~dirtyDeployed                                         \* no_dirty_deploy
TypeOK == lock \in Sess \cup {0}

\* ---- witnesses (MUST FAIL cfgs: the state is reachable) ----
NotAllLive == ~(\A r \in RT : AllF \subseteq prodC[r])                  \* all_features_live
NeverRefused == ~refused                                                \* guard_refused
=============================================================================
