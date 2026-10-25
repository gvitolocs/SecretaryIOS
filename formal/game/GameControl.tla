----------------------------- MODULE GameControl -----------------------------
(***************************************************************************)
(* Lucina's Pokémon Red game control (Lucina repo, rust/lucina-pokemon,    *)
(* branch lucina-game-link: main.rs server()/handle(), macros.rs frames,   *)
(* tap, step). One emulator behind a mutex. The clock thread advances a    *)
(* frame while no macro runs (and checkpoints the save); macro callers     *)
(* take `busy` by compare-and-swap, press a key without a pause check,     *)
(* advance frames (each checks `running` under the lock and aborts,        *)
(* lifting the key), then save and clear `busy`. Pause/play comes from     *)
(* chat, the iPhone and web screens, the Mac viewer over a tunnel that can *)
(* drop, and the play loop's auto-pause; /save saves; a restart kills      *)
(* every thread and comes back paused. Every action is one critical        *)
(* section of the emulator lock, except the current clock thread, which    *)
(* reads `busy` before taking the lock (ClockRead, then ClockFrame).       *)
(* TLA+ twin of `Game` in verification/models/src/game.rs.                 *)
(***************************************************************************)
EXTENDS Integers, FiniteSets

CONSTANTS Budget,     \* macro calls + control/save requests
          Frames,     \* frames per macro
          FixClock,   \* Pk1: the clock checks `busy` under the lock
          FixSave     \* Pk2: /control and /save do not save while `busy`

Workers == {0, 1}
Sources == {"chat", "iphone", "web", "viewer"}
NoOne == -1

VARIABLES running, busy, key, phase, left, clock, inBattle, tunnel,
          inputFramesPaused, clockFrameDuringInput, saveDuringInput,
          pausedInBattle, resumedInBattle, budget, restarts
vars == <<running, busy, key, phase, left, clock, inBattle, tunnel,
          inputFramesPaused, clockFrameDuringInput, saveDuringInput,
          pausedInBattle, resumedInBattle, budget, restarts>>

Init == /\ running = TRUE /\ busy = NoOne /\ key = NoOne
        /\ phase = [w \in Workers |-> "idle"] /\ left = [w \in Workers |-> 0]
        /\ clock = "idle" /\ inBattle = FALSE /\ tunnel = TRUE
        /\ inputFramesPaused = 0 /\ clockFrameDuringInput = FALSE /\ saveDuringInput = FALSE
        /\ pausedInBattle = FALSE /\ resumedInBattle = FALSE
        /\ budget = Budget /\ restarts = 1

\* A save under the lock: bad if a key is down.
SaveNow == saveDuringInput' = (saveDuringInput \/ key # NoOne)
HandlerSave == IF ~FixSave \/ busy = NoOne THEN SaveNow ELSE UNCHANGED saveDuringInput
\* Bookkeeping of a frame advanced by the clock thread.
ClockFrameEffects ==
    /\ clockFrameDuringInput' = (clockFrameDuringInput \/ key # NoOne)
    /\ inputFramesPaused' = IF key # NoOne /\ ~running THEN inputFramesPaused + 1 ELSE inputFramesPaused

ClockRead ==
    /\ clock = "idle"
    /\ IF FixClock
       THEN /\ IF busy = NoOne
               THEN /\ ClockFrameEffects
                    /\ \/ SaveNow
                       \/ UNCHANGED saveDuringInput
               ELSE UNCHANGED <<clockFrameDuringInput, inputFramesPaused, saveDuringInput>>
            /\ UNCHANGED clock
       ELSE /\ busy = NoOne
            /\ clock' = "sawFree"
            /\ UNCHANGED <<clockFrameDuringInput, inputFramesPaused, saveDuringInput>>
    /\ UNCHANGED <<running, busy, key, phase, left, inBattle, tunnel, pausedInBattle, resumedInBattle, budget, restarts>>

ClockFrame ==
    /\ clock = "sawFree"
    /\ ClockFrameEffects
    /\ \/ SaveNow                 \* the 30 s checkpoint
       \/ UNCHANGED saveDuringInput
    /\ clock' = "idle"
    /\ UNCHANGED <<running, busy, key, phase, left, inBattle, tunnel, pausedInBattle, resumedInBattle, budget, restarts>>

MacroStart(w) ==
    /\ phase[w] = "idle" /\ budget > 0
    /\ budget' = budget - 1
    /\ IF busy = NoOne
       THEN /\ busy' = w /\ phase' = [phase EXCEPT ![w] = "running"] /\ left' = [left EXCEPT ![w] = Frames]
       ELSE UNCHANGED <<busy, phase, left>>     \* "Another action is still running."
    /\ UNCHANGED <<running, key, clock, inBattle, tunnel, inputFramesPaused, clockFrameDuringInput,
                   saveDuringInput, pausedInBattle, resumedInBattle, restarts>>

\* with(|g| g.key_press(..)): no pause check.
Press(w) ==
    /\ phase[w] = "running" /\ key = NoOne
    /\ key' = w
    /\ resumedInBattle' = (resumedInBattle \/ (inBattle /\ running /\ pausedInBattle))
    /\ UNCHANGED <<running, busy, phase, left, clock, inBattle, tunnel, inputFramesPaused,
                   clockFrameDuringInput, saveDuringInput, pausedInBattle, budget, restarts>>

\* frames(1): under the lock, "Paused by the owner" when ~running (the caller lifts its key).
Frame(w) ==
    /\ phase[w] = "running"
    /\ IF ~running
       THEN /\ key' = IF key = w THEN NoOne ELSE key
            /\ phase' = [phase EXCEPT ![w] = "ending"]
            /\ UNCHANGED <<left, inBattle>>
       ELSE /\ inBattle' = (inBattle \/ (key = w /\ left[w] % 2 = 0))
            /\ IF left[w] <= 1
               THEN /\ phase' = [phase EXCEPT ![w] = "ending"]
                    /\ key' = IF key = w THEN NoOne ELSE key
               ELSE /\ phase' = phase /\ key' = key
            /\ left' = [left EXCEPT ![w] = left[w] - 1]
    /\ UNCHANGED <<running, busy, clock, tunnel, inputFramesPaused, clockFrameDuringInput,
                   saveDuringInput, pausedInBattle, resumedInBattle, budget, restarts>>

Lift(w) ==
    /\ phase[w] = "running" /\ key = w
    /\ key' = NoOne
    /\ UNCHANGED <<running, busy, phase, left, clock, inBattle, tunnel, inputFramesPaused,
                   clockFrameDuringInput, saveDuringInput, pausedInBattle, resumedInBattle, budget, restarts>>

\* Save, journal, observation, then busy = false.
End(w) ==
    /\ phase[w] = "ending"
    /\ SaveNow
    /\ busy' = NoOne /\ phase' = [phase EXCEPT ![w] = "idle"]
    /\ UNCHANGED <<running, key, left, clock, inBattle, tunnel, inputFramesPaused,
                   clockFrameDuringInput, pausedInBattle, resumedInBattle, budget, restarts>>

\* POST /control from chat, the iPhone, the web or the viewer (only while its tunnel is up).
Control(src, run) ==
    /\ budget > 0 /\ (src # "viewer" \/ tunnel)
    /\ budget' = budget - 1
    /\ running' = run
    /\ inputFramesPaused' = 0
    /\ pausedInBattle' = (pausedInBattle \/ (~run /\ inBattle))
    /\ HandlerSave
    /\ UNCHANGED <<busy, key, phase, left, clock, inBattle, tunnel, clockFrameDuringInput,
                   resumedInBattle, restarts>>

\* The play loop pauses itself after three failed Hermes turns.
AutoPause == running /\ Control("chat", FALSE)

SaveReq ==
    /\ budget > 0 /\ budget' = budget - 1
    /\ HandlerSave
    /\ UNCHANGED <<running, busy, key, phase, left, clock, inBattle, tunnel, inputFramesPaused,
                   clockFrameDuringInput, pausedInBattle, resumedInBattle, restarts>>

Tunnel ==
    /\ tunnel' = ~tunnel
    /\ UNCHANGED <<running, busy, key, phase, left, clock, inBattle, inputFramesPaused,
                   clockFrameDuringInput, saveDuringInput, pausedInBattle, resumedInBattle, budget, restarts>>

BattleEnds ==
    /\ inBattle /\ inBattle' = FALSE
    /\ UNCHANGED <<running, busy, key, phase, left, clock, tunnel, inputFramesPaused,
                   clockFrameDuringInput, saveDuringInput, pausedInBattle, resumedInBattle, budget, restarts>>

\* The game server restarts: every thread dies, `running` is not persisted.
Restart ==
    /\ restarts > 0 /\ restarts' = restarts - 1
    /\ running' = FALSE /\ inputFramesPaused' = 0
    /\ busy' = NoOne /\ key' = NoOne /\ clock' = "idle"
    /\ phase' = [w \in Workers |-> "idle"]
    /\ UNCHANGED <<left, inBattle, tunnel, clockFrameDuringInput, saveDuringInput,
                   pausedInBattle, resumedInBattle, budget>>

Next == \/ ClockRead \/ ClockFrame
        \/ \E w \in Workers : MacroStart(w) \/ Press(w) \/ Frame(w) \/ Lift(w) \/ End(w)
        \/ \E src \in Sources, run \in BOOLEAN : Control(src, run)
        \/ AutoPause \/ SaveReq \/ Tunnel \/ BattleEnds \/ Restart

Spec == Init /\ [][Next]_vars

\* ---------------------------------------------------------------- properties
OneDriver == Cardinality({w \in Workers : phase[w] # "idle"}) <= 1
KeysOnlyByBusyHolder == key = NoOne \/ key = busy
NoClockFrameDuringInput == ~clockFrameDuringInput
PauseWithinOneStep == inputFramesPaused <= 1
NoSaveDuringInput == ~saveDuringInput
\* Witness (MUST FAIL in the *_witness cfg): a pause during a battle, then input again in that battle.
NeverResumedInBattle == ~resumedInBattle
=============================================================================
