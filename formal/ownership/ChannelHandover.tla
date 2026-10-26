--------------------------- MODULE ChannelHandover ---------------------------
(***************************************************************************)
(* Phases 3 and 4 (docs/lucina-rust/PORT_PLAN.md): Telegram getUpdates     *)
(* behind the gateway lock (telegram-<botid>.lock, same record in both     *)
(* runtimes) and the Discord gateway, which Node does not lock. TLA+ twin  *)
(* of the `Channels` model in verification/models/src/ownership.rs.        *)
(***************************************************************************)
EXTENDS Integers, Sequences

CONSTANTS PlanName,      \* "documented" | "discord_wrong_order" | "lock_disabled"
          Crashes

Rts == {"node", "rust"}
S(rt, tg, dc) == [rt |-> rt, tg |-> tg, dc |-> dc]
Documented == << S("rust", TRUE, FALSE),    \* phase 3 (1): Rust poller in standby
                 S("node", FALSE, TRUE),    \* (2) Node restarted without the token (SIGTERM releases the lock)
                 S("node", FALSE, FALSE),   \* phase 4: Node without Discord first ...
                 S("rust", TRUE, TRUE),     \* ... then Rust connects
                 S("rust", FALSE, FALSE),   \* rollback: stop Rust first
                 S("node", TRUE, TRUE) >>   \* then restore Node
WrongOrder == << S("rust", FALSE, TRUE), S("node", TRUE, FALSE) >>
Plan == CASE PlanName = "documented" -> Documented
          [] PlanName = "discord_wrong_order" -> WrongOrder
          [] PlanName = "lock_disabled" -> << S("rust", TRUE, FALSE), S("node", FALSE, TRUE) >>
LockDisabled == PlanName = "lock_disabled"   \* HERMES_GATEWAY_LOCK=false (forbidden)

VARIABLES env, lock, pos, crashesLeft
vars == <<env, lock, pos, crashesLeft>>

PollsTelegram(rt) == env[rt].up /\ env[rt].tg /\ (LockDisabled \/ lock = rt)
OnDiscord(rt) == env[rt].up /\ env[rt].dc

Init == /\ env = [node |-> [up |-> TRUE, tg |-> TRUE, dc |-> TRUE],
                  rust |-> [up |-> TRUE, tg |-> FALSE, dc |-> FALSE]]
        /\ lock = "node" /\ pos = 1 /\ crashesLeft = Crashes

Operator == /\ pos <= Len(Plan)
            /\ LET st == Plan[pos]
               IN /\ env' = [env EXCEPT ![st.rt] = [up |-> TRUE, tg |-> st.tg, dc |-> st.dc]]
                  /\ lock' = IF lock = st.rt THEN "none" ELSE lock   \* SIGTERM releases it
            /\ pos' = pos + 1 /\ UNCHANGED crashesLeft
Kill(rt) == /\ env[rt].up /\ crashesLeft > 0     \* SIGKILL: the lock stays live for 90 s
            /\ env' = [env EXCEPT ![rt].up = FALSE] /\ crashesLeft' = crashesLeft - 1
            /\ UNCHANGED <<lock, pos>>
Start(rt) == /\ ~env[rt].up /\ env' = [env EXCEPT ![rt].up = TRUE] /\ UNCHANGED <<lock, pos, crashesLeft>>
LockExpires == /\ lock \in Rts /\ ~env[lock].up /\ lock' = "none" /\ UNCHANGED <<env, pos, crashesLeft>>
Acquire(rt) == /\ env[rt].up /\ env[rt].tg /\ lock = "none" /\ lock' = rt /\ UNCHANGED <<env, pos, crashesLeft>>

Next == Operator \/ LockExpires \/ \E rt \in Rts : Kill(rt) \/ Start(rt) \/ Acquire(rt)
Spec == Init /\ [][Next]_vars

SingleTelegramPoller == ~(PollsTelegram("node") /\ PollsTelegram("rust"))
SingleDiscordGateway == ~(OnDiscord("node") /\ OnDiscord("rust"))
RustNeverPolls == ~PollsTelegram("rust")
=============================================================================
