------------------------------ MODULE Meetings ------------------------------
(***************************************************************************)
(* Meetings end to end (docs/meetings.md): the Mac uploader (create with   *)
(* an Idempotency-Key, offset query, chunks), a network that reorders,     *)
(* duplicates and cuts PUTs, the /v1/meetings store (kernel MT2 chunk,     *)
(* SHA-256 check, MT3 may_complete) and the lucina-memo meetings worker    *)
(* (transcribe under the GPU lease, diarize, analyze + brief with the      *)
(* deterministic fallback, files, note, memory, notification, voice        *)
(* question), worker crashes, another GPU job, the owner naming a speaker. *)
(* TLA+ twin of verification/models/src/meetings.rs (same actions, same    *)
(* invariants; liveness is checked in the Stateright model only).          *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS FixOffsetCheck,    \* MT2: append only at the stored offset
          FixIdempotency,    \* a retried create returns the same meeting
          FixCompleteCheck,  \* MT3: complete only when the track is whole
          FixMarkers,        \* MT6/MT8: marker before Telegram, memory, voice question
          CreateRetries, NetFaults, Crashes, LlmFails, OtherJobs, OwnerActs, MaxAttempts

Size == 2
Statuses == {"uploading", "queued", "transcribing", "diarizing", "analyzing", "publishing", "done"}

VARIABLES hasId, belief, confirmed, inFlight,
          meetings, stored, whole, status, badTransition,
          transcribed, diarized, analyzed, written, noted, remembered, notified, askedDone,
          memMarker, tgMarker, askMarker, step, lease, attempts, fallback,
          memoryEntries, telegram, questions, cutSeen, sawCutThenWhole, gpuWasBusy,
          createRetries, netFaults, crashes, llmFails, otherJobs, ownerActs

vars == <<hasId, belief, confirmed, inFlight,
          meetings, stored, whole, status, badTransition,
          transcribed, diarized, analyzed, written, noted, remembered, notified, askedDone,
          memMarker, tgMarker, askMarker, step, lease, attempts, fallback,
          memoryEntries, telegram, questions, cutSeen, sawCutThenWhole, gpuWasBusy,
          createRetries, netFaults, crashes, llmFails, otherJobs, ownerActs>>

\* MT1 (without "failed").
Allowed(from, to) == \/ from = to
                     \/ from = "uploading" /\ to = "queued"
                     \/ from = "done" /\ to = "queued"
                     \/ from \notin {"uploading", "done"} /\ to # "uploading"

Init == /\ hasId = FALSE /\ belief = 0 /\ confirmed = FALSE /\ inFlight = [x \in 0..Size-1 |-> 0]
        /\ meetings = 0 /\ stored = <<>> /\ whole = FALSE /\ status = "uploading" /\ badTransition = FALSE
        /\ transcribed = FALSE /\ diarized = FALSE /\ analyzed = FALSE /\ written = FALSE
        /\ noted = FALSE /\ remembered = FALSE /\ notified = FALSE /\ askedDone = FALSE
        /\ memMarker = FALSE /\ tgMarker = FALSE /\ askMarker = FALSE /\ step = "idle"
        /\ lease = "free" /\ attempts = 0 /\ fallback = FALSE
        /\ memoryEntries = 0 /\ telegram = 0 /\ questions = 0
        /\ cutSeen = FALSE /\ sawCutThenWhole = FALSE /\ gpuWasBusy = FALSE
        /\ createRetries = CreateRetries /\ netFaults = NetFaults /\ crashes = Crashes
        /\ llmFails = LlmFails /\ otherJobs = OtherJobs /\ ownerActs = OwnerActs

WorkerVars == <<transcribed, diarized, analyzed, written, noted, remembered, notified, askedDone,
                memMarker, tgMarker, askMarker, step, lease, attempts, fallback,
                memoryEntries, telegram, questions, gpuWasBusy, crashes, llmFails, otherJobs, ownerActs>>
UploadVars == <<hasId, belief, confirmed, inFlight, meetings, stored, whole, cutSeen, sawCutThenWhole,
                createRetries, netFaults>>

SetStatus(to) == /\ status' = to /\ badTransition' = (badTransition \/ ~Allowed(status, to))
KeepStatus == UNCHANGED <<status, badTransition>>

\* ---------------------------------------------------------------- the Mac and the network
Create == /\ ~hasId /\ hasId' = TRUE /\ meetings' = meetings + 1
          /\ UNCHANGED <<belief, confirmed, inFlight, stored, whole, cutSeen, sawCutThenWhole, createRetries, netFaults>>
          /\ KeepStatus /\ UNCHANGED WorkerVars
RetryCreate == /\ hasId /\ createRetries > 0 /\ createRetries' = createRetries - 1
               /\ meetings' = IF FixIdempotency THEN meetings ELSE meetings + 1
               /\ UNCHANGED <<hasId, belief, confirmed, inFlight, stored, whole, cutSeen, sawCutThenWhole, netFaults>>
               /\ KeepStatus /\ UNCHANGED WorkerVars
Query == /\ hasId /\ ~confirmed /\ belief' = Len(stored) /\ confirmed' = whole
         /\ UNCHANGED <<hasId, inFlight, meetings, stored, whole, cutSeen, sawCutThenWhole, createRetries, netFaults>>
         /\ KeepStatus /\ UNCHANGED WorkerVars
Send == /\ hasId /\ ~confirmed /\ belief < Size /\ inFlight[belief] = 0
        /\ inFlight' = [inFlight EXCEPT ![belief] = 1] /\ belief' = belief + 1
        /\ UNCHANGED <<hasId, confirmed, meetings, stored, whole, cutSeen, sawCutThenWhole, createRetries, netFaults>>
        /\ KeepStatus /\ UNCHANGED WorkerVars
Duplicate(x) == /\ inFlight[x] > 0 /\ netFaults > 0 /\ netFaults' = netFaults - 1
                /\ inFlight' = [inFlight EXCEPT ![x] = @ + 1]
                /\ UNCHANGED <<hasId, belief, confirmed, meetings, stored, whole, cutSeen, sawCutThenWhole, createRetries>>
                /\ KeepStatus /\ UNCHANGED WorkerVars
Cut(x) == /\ inFlight[x] > 0 /\ netFaults > 0 /\ netFaults' = netFaults - 1
          /\ inFlight' = [inFlight EXCEPT ![x] = @ - 1] /\ cutSeen' = TRUE
          /\ UNCHANGED <<hasId, belief, confirmed, meetings, stored, whole, sawCutThenWhole, createRetries>>
          /\ KeepStatus /\ UNCHANGED WorkerVars

IsPrefix(s) == \A i \in 1..Len(s) : s[i] = i - 1
Appends(x) == IF FixOffsetCheck THEN ~whole /\ x = Len(stored) /\ x < Size
              ELSE ~whole /\ Len(stored) < Size /\ x < Size

Deliver(x) == /\ inFlight[x] > 0
              /\ inFlight' = [inFlight EXCEPT ![x] = @ - 1]
              /\ IF Appends(x)
                    THEN LET s2 == Append(stored, x) IN
                         IF Len(s2) = Size
                            THEN IF IsPrefix(s2)   \* the SHA-256 check
                                    THEN /\ stored' = s2 /\ whole' = TRUE /\ confirmed' = TRUE
                                         /\ sawCutThenWhole' = (sawCutThenWhole \/ cutSeen)
                                    ELSE /\ stored' = <<>> /\ UNCHANGED <<whole, confirmed, sawCutThenWhole>>
                            ELSE /\ stored' = s2 /\ UNCHANGED <<whole, confirmed, sawCutThenWhole>>
                    ELSE UNCHANGED <<stored, whole, confirmed, sawCutThenWhole>>
              /\ UNCHANGED <<hasId, belief, meetings, cutSeen, createRetries, netFaults>>
              /\ KeepStatus /\ UNCHANGED WorkerVars

Complete == /\ hasId /\ status = "uploading" /\ (whole \/ ~FixCompleteCheck)
            /\ SetStatus("queued")
            /\ UNCHANGED UploadVars /\ UNCHANGED WorkerVars

\* ---------------------------------------------------------------- the GPU and the owner
OtherTakesGpu == /\ otherJobs > 0 /\ lease = "free" /\ otherJobs' = otherJobs - 1 /\ lease' = "other"
                 /\ UNCHANGED <<transcribed, diarized, analyzed, written, noted, remembered, notified, askedDone,
                                memMarker, tgMarker, askMarker, step, attempts, fallback,
                                memoryEntries, telegram, questions, gpuWasBusy, crashes, llmFails, ownerActs>>
                 /\ KeepStatus /\ UNCHANGED UploadVars
FreeLease == /\ lease \in {"other", "dead"} /\ lease' = "free"
             /\ UNCHANGED <<transcribed, diarized, analyzed, written, noted, remembered, notified, askedDone,
                            memMarker, tgMarker, askMarker, step, attempts, fallback,
                            memoryEntries, telegram, questions, gpuWasBusy, crashes, llmFails, otherJobs, ownerActs>>
             /\ KeepStatus /\ UNCHANGED UploadVars
OwnerNamesSpeaker == /\ status = "done" /\ ownerActs > 0 /\ ownerActs' = ownerActs - 1
                     /\ written' = FALSE /\ noted' = FALSE /\ SetStatus("queued")
                     /\ UNCHANGED <<transcribed, diarized, analyzed, remembered, notified, askedDone,
                                    memMarker, tgMarker, askMarker, step, lease, attempts, fallback,
                                    memoryEntries, telegram, questions, gpuWasBusy, crashes, llmFails, otherJobs>>
                     /\ UNCHANGED UploadVars

\* ---------------------------------------------------------------- the worker
Pipeline == status \notin {"uploading", "done"}

WorkerCrash == /\ crashes > 0 /\ (Pipeline \/ step # "idle")
               /\ crashes' = crashes - 1 /\ step' = "idle"
               /\ lease' = IF lease = "worker" THEN "dead" ELSE lease
               /\ UNCHANGED <<transcribed, diarized, analyzed, written, noted, remembered, notified, askedDone,
                              memMarker, tgMarker, askMarker, attempts, fallback,
                              memoryEntries, telegram, questions, gpuWasBusy, llmFails, otherJobs, ownerActs>>
               /\ KeepStatus /\ UNCHANGED UploadVars

LlmFailure == /\ Pipeline /\ step = "idle" /\ diarized /\ ~analyzed /\ llmFails > 0
              /\ llmFails' = llmFails - 1 /\ attempts' = attempts + 1 /\ SetStatus("queued")
              /\ UNCHANGED <<transcribed, diarized, analyzed, written, noted, remembered, notified, askedDone,
                             memMarker, tgMarker, askMarker, step, lease, fallback,
                             memoryEntries, telegram, questions, gpuWasBusy, crashes, otherJobs, ownerActs>>
              /\ UNCHANGED UploadVars

FinishStep == /\ step # "idle" /\ step' = "idle"
              /\ CASE step = "whisper"    -> lease' = "free" /\ transcribed' = TRUE
                                             /\ UNCHANGED <<remembered, notified, askedDone>>
                   [] step = "remembered" -> remembered' = TRUE /\ UNCHANGED <<lease, transcribed, notified, askedDone>>
                   [] step = "notified"   -> notified' = TRUE /\ UNCHANGED <<lease, transcribed, remembered, askedDone>>
                   [] step = "asked"      -> askedDone' = TRUE /\ UNCHANGED <<lease, transcribed, remembered, notified>>
              /\ UNCHANGED <<diarized, analyzed, written, noted, memMarker, tgMarker, askMarker, attempts, fallback,
                             memoryEntries, telegram, questions, gpuWasBusy, crashes, llmFails, otherJobs, ownerActs>>
              /\ KeepStatus /\ UNCHANGED UploadVars

Same == UNCHANGED <<crashes, llmFails, otherJobs, ownerActs, attempts>> /\ UNCHANGED UploadVars

StartStep ==
  /\ Pipeline /\ step = "idle" /\ Same
  /\ CASE ~transcribed ->
            IF lease = "free"
               THEN /\ SetStatus("transcribing") /\ lease' = "worker" /\ step' = "whisper"
                    /\ UNCHANGED <<transcribed, diarized, analyzed, written, noted, remembered, notified, askedDone,
                                   memMarker, tgMarker, askMarker, fallback, memoryEntries, telegram, questions, gpuWasBusy>>
               ELSE /\ ~gpuWasBusy /\ gpuWasBusy' = TRUE /\ SetStatus("queued")   \* else: wait (no step)
                    /\ UNCHANGED <<transcribed, diarized, analyzed, written, noted, remembered, notified, askedDone,
                                   memMarker, tgMarker, askMarker, step, lease, fallback, memoryEntries, telegram, questions>>
       [] transcribed /\ ~diarized ->
            /\ SetStatus("diarizing") /\ diarized' = TRUE
            /\ UNCHANGED <<transcribed, analyzed, written, noted, remembered, notified, askedDone,
                           memMarker, tgMarker, askMarker, step, lease, fallback, memoryEntries, telegram, questions, gpuWasBusy>>
       [] diarized /\ ~analyzed ->
            /\ SetStatus("analyzing") /\ analyzed' = TRUE /\ fallback' = (attempts + 1 >= MaxAttempts)
            /\ UNCHANGED <<transcribed, diarized, written, noted, remembered, notified, askedDone,
                           memMarker, tgMarker, askMarker, step, lease, memoryEntries, telegram, questions, gpuWasBusy>>
       [] analyzed /\ ~written ->
            /\ SetStatus("publishing") /\ written' = TRUE
            /\ UNCHANGED <<transcribed, diarized, analyzed, noted, remembered, notified, askedDone,
                           memMarker, tgMarker, askMarker, step, lease, fallback, memoryEntries, telegram, questions, gpuWasBusy>>
       [] written /\ ~noted ->
            /\ SetStatus("publishing") /\ noted' = TRUE
            /\ UNCHANGED <<transcribed, diarized, analyzed, written, remembered, notified, askedDone,
                           memMarker, tgMarker, askMarker, step, lease, fallback, memoryEntries, telegram, questions, gpuWasBusy>>
       [] noted /\ ~remembered ->
            /\ SetStatus("publishing") /\ step' = "remembered"
            /\ memMarker' = (memMarker \/ FixMarkers)
            /\ memoryEntries' = IF ~FixMarkers \/ ~memMarker THEN memoryEntries + 1 ELSE memoryEntries
            /\ UNCHANGED <<transcribed, diarized, analyzed, written, noted, remembered, notified, askedDone,
                           tgMarker, askMarker, lease, fallback, telegram, questions, gpuWasBusy>>
       [] remembered /\ ~notified ->
            /\ SetStatus("publishing") /\ step' = "notified"
            /\ tgMarker' = (tgMarker \/ FixMarkers)
            /\ telegram' = IF ~FixMarkers \/ ~tgMarker THEN telegram + 1 ELSE telegram
            /\ UNCHANGED <<transcribed, diarized, analyzed, written, noted, remembered, notified, askedDone,
                           memMarker, askMarker, lease, fallback, memoryEntries, questions, gpuWasBusy>>
       [] notified /\ ~askedDone ->
            /\ KeepStatus /\ step' = "asked"
            /\ askMarker' = (askMarker \/ FixMarkers)
            /\ questions' = IF ~FixMarkers \/ ~askMarker THEN questions + 1 ELSE questions
            /\ UNCHANGED <<transcribed, diarized, analyzed, written, noted, remembered, notified, askedDone,
                           memMarker, tgMarker, lease, fallback, memoryEntries, telegram, gpuWasBusy>>
       [] OTHER ->
            /\ SetStatus("done")
            /\ UNCHANGED <<transcribed, diarized, analyzed, written, noted, remembered, notified, askedDone,
                           memMarker, tgMarker, askMarker, step, lease, fallback, memoryEntries, telegram, questions, gpuWasBusy>>

Next == \/ Create \/ RetryCreate \/ Query \/ Send \/ Complete
        \/ \E x \in 0..Size-1 : Deliver(x) \/ Duplicate(x) \/ Cut(x)
        \/ OtherTakesGpu \/ FreeLease \/ OwnerNamesSpeaker
        \/ WorkerCrash \/ LlmFailure \/ FinishStep \/ StartStep

Spec == Init /\ [][Next]_vars

\* ---------------------------------------------------------------- invariants
StoredIsPrefix == IsPrefix(stored) /\ Len(stored) <= Size
OneMeetingPerRecording == meetings <= 1
QueuedOnlyWhenWhole == status = "uploading" \/ whole
StatusTransitionsAllowed == ~badTransition
TelegramAtMostOnce == telegram <= 1
MemoryAtMostOnce == memoryEntries <= 1
VoiceAskedAtMostOnce == questions <= 1
DoneHasEveryOutput == status = "done" => whole /\ transcribed /\ diarized /\ analyzed /\ written
                                         /\ noted /\ remembered /\ notified

\* Witness targets (a MUST FAIL cfg shows the state is reached).
NeverResumedAfterCut == ~(sawCutThenWhole /\ status = "done")
NeverFallbackDone == ~(fallback /\ status = "done")
NeverGpuBusyThenDone == ~(gpuWasBusy /\ status = "done")
=============================================================================
