------------------------------- MODULE Memos -------------------------------
(***************************************************************************)
(* Voice memos end to end (docs/memos.md): the iPhone recorder and Live    *)
(* Activity, the upload queue, the lucina-memo worker (transcript, summary,*)
(* PDF, Obsidian push, notification), the GPU lease, owner rename and      *)
(* reprocess, iOS killing the app, worker crashes, speaker labels (K11); *)
(* the calendar link                                                      *)
(* (2026-10-26): the worker links the overlapping event unless the owner  *)
(* chose one, the owner picks another or none, a linked note lives in the *)
(* event's folder and moves when the event changes.                       *)
(* TLA+ twin of verification/models/src/memos.rs (same actions, same       *)
(* invariants; liveness is checked in the Stateright model only).          *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS FixNotePath,        \* kernel K7: record the note path before the rsync
          FixNoDowngrade,     \* kernel K1: never trade a Whisper transcript for the iPhone's
          FixTelegramMarker,  \* kernel K6: record the Telegram revision before sending
          FixRecovery,        \* the iPhone recovers a recording cut off by iOS
          PruneDead,          \* gpu-lease drops a dead holder
          FixDiarizeReset,    \* kernel K11: a new transcript clears the diarization attempt
          FixOldNotes,       \* every earlier note path is kept for deletion (B5: one slot)
          FixLinkRecheck,     \* the link write re-checks the owner's choice (kernel K9)
          Events,             \* calendar linking on
          LateEvent,          \* the event reaches the server only after the memo was processed
          FixRelink,          \* the re-link job links it then, with a quiet re-summary (2026-10-28)
          Kills, Crashes, NetFails, OwnerActs, OtherJobs, EventActs, GiveUps

VARIABLES rec, appAlive, la, queued, uploaded, devSent,
          transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, summarized, rendered, noteRec, published,
          notified, tgMarker, done, revision, title, step, lease, gpuWasBusy,
          vault, telegram, inbox,
          kills, crashes, netFails, ownerActs, otherJobs, toggles,
          oldNotes, event, eventSrc, linked, ownerChoice, failed, eventActs, giveUps, calendarHas, relinked

vars == <<rec, appAlive, la, queued, uploaded, devSent,
          transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, summarized, rendered, noteRec, published,
          notified, tgMarker, done, revision, title, step, lease, gpuWasBusy,
          vault, telegram, inbox,
          kills, crashes, netFails, ownerActs, otherJobs, toggles,
          oldNotes, event, eventSrc, linked, ownerChoice, failed, eventActs, giveUps, calendarHas, relinked>>

None == -1   \* no marker / no owner choice
NoNote == <<>>   \* no recorded note path; a path is <<folder, name>>
\* Note folders (event = Root: no event linked).
Root == 0
Course == 1
Meeting == 2

EventVars == <<oldNotes, event, eventSrc, linked, ownerChoice, failed, eventActs, giveUps, calendarHas, relinked>>

Init == /\ rec = "recording" /\ appAlive = TRUE /\ la = "running"
        /\ queued = FALSE /\ uploaded = FALSE /\ devSent = FALSE
        /\ transcript = "none" /\ diarized = "none" /\ diarAttempted = FALSE /\ hadWhisper = FALSE /\ sawUpgrade = FALSE
        /\ summarized = FALSE /\ rendered = FALSE /\ noteRec = NoNote /\ published = FALSE
        /\ notified = FALSE /\ tgMarker = None /\ done = FALSE /\ revision = 0 /\ title = 0
        /\ step = "idle" /\ lease = "free" /\ gpuWasBusy = FALSE
        /\ vault = {} /\ telegram = 0 /\ inbox = <<>>
        /\ kills = Kills /\ crashes = Crashes /\ netFails = NetFails
        /\ ownerActs = OwnerActs /\ otherJobs = OtherJobs /\ toggles = 2
        /\ oldNotes = {} /\ event = Root /\ eventSrc = "none" /\ linked = FALSE
        /\ ownerChoice = None /\ failed = FALSE /\ eventActs = EventActs /\ giveUps = GiveUps
        /\ calendarHas = ~LateEvent /\ relinked = FALSE

Pending == IF ~uploaded \/ done \/ failed THEN 0
           ELSE IF transcript = "none" THEN 1
           ELSE IF Events /\ ~linked THEN 10
           ELSE IF ~diarAttempted THEN 11
           ELSE IF ~summarized THEN 2
           ELSE IF ~rendered THEN 3
           ELSE IF ~published THEN 4
           ELSE IF ~notified THEN 5 ELSE 6

ServerVars == <<transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, summarized, rendered, noteRec, published,
                notified, tgMarker, done, revision, title, step, lease, gpuWasBusy,
                vault, telegram, inbox, crashes, ownerActs, otherJobs>>

\* ---------------------------------------------------------------- the iPhone
TogglePause == /\ appAlive /\ rec \in {"recording", "paused"} /\ toggles > 0
               /\ toggles' = toggles - 1
               /\ rec' = IF rec = "recording" THEN "paused" ELSE "recording"
               /\ la' = IF rec = "recording" THEN "paused" ELSE "running"
               /\ UNCHANGED <<appAlive, queued, uploaded, devSent, kills, netFails>>
               /\ UNCHANGED ServerVars
               /\ UNCHANGED EventVars
Stop == /\ appAlive /\ rec \in {"recording", "paused"}
        /\ rec' = "saved" /\ la' = "none" /\ queued' = TRUE
        /\ UNCHANGED <<appAlive, uploaded, devSent, kills, netFails, toggles>> /\ UNCHANGED ServerVars
        /\ UNCHANGED EventVars
KillApp == /\ appAlive /\ kills > 0 /\ rec # "lost"
           /\ kills' = kills - 1 /\ appAlive' = FALSE
           /\ rec' = IF rec \in {"recording", "paused"} THEN "killed" ELSE rec
           /\ UNCHANGED <<la, queued, uploaded, devSent, netFails, toggles>> /\ UNCHANGED ServerVars
           /\ UNCHANGED EventVars
Relaunch == /\ ~appAlive /\ appAlive' = TRUE /\ la' = "none"
            /\ IF rec = "killed"
                  THEN IF FixRecovery THEN rec' = "recovered" /\ queued' = TRUE
                                      ELSE rec' = "lost" /\ UNCHANGED queued
                  ELSE UNCHANGED <<rec, queued>>
            /\ UNCHANGED <<uploaded, devSent, kills, netFails, toggles>> /\ UNCHANGED ServerVars
            /\ UNCHANGED EventVars
UploadOk == /\ appAlive /\ queued /\ ~uploaded /\ uploaded' = TRUE
            /\ UNCHANGED <<rec, appAlive, la, queued, devSent, kills, netFails, toggles>> /\ UNCHANGED ServerVars
            /\ UNCHANGED EventVars
UploadFails == /\ appAlive /\ queued /\ ~uploaded /\ netFails > 0 /\ netFails' = netFails - 1
               /\ UNCHANGED <<rec, appAlive, la, queued, uploaded, devSent, kills, toggles>> /\ UNCHANGED ServerVars
               /\ UNCHANGED EventVars
SendDeviceTranscript == /\ appAlive /\ queued /\ uploaded /\ ~devSent /\ devSent' = TRUE
                        /\ UNCHANGED <<rec, appAlive, la, queued, uploaded, kills, netFails, toggles>>
                        /\ UNCHANGED ServerVars
                        /\ UNCHANGED EventVars

PhoneVars == <<rec, appAlive, la, queued, uploaded, devSent, kills, netFails, toggles>>

\* ---------------------------------------------------------------- the GPU and the owner
OtherTakesGpu == /\ otherJobs > 0 /\ lease = "free" /\ otherJobs' = otherJobs - 1 /\ lease' = "other"
                 /\ UNCHANGED PhoneVars
                 /\ UNCHANGED <<transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, summarized, rendered, noteRec, published,
                                notified, tgMarker, done, revision, title, step, gpuWasBusy, vault,
                                telegram, inbox, crashes, ownerActs>>
                 /\ UNCHANGED EventVars
OtherReleasesGpu == /\ lease = "other" /\ lease' = "free"
                    /\ UNCHANGED PhoneVars
                    /\ UNCHANGED <<transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, summarized, rendered, noteRec, published,
                                   notified, tgMarker, done, revision, title, step, gpuWasBusy, vault,
                                   telegram, inbox, crashes, ownerActs, otherJobs>>
                    /\ UNCHANGED EventVars
PruneDeadLease == /\ PruneDead /\ lease = "dead" /\ lease' = "free"
                  /\ UNCHANGED PhoneVars
                  /\ UNCHANGED <<transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, summarized, rendered, noteRec, published,
                                 notified, tgMarker, done, revision, title, step, gpuWasBusy, vault,
                                 telegram, inbox, crashes, ownerActs, otherJobs>>
                  /\ UNCHANGED EventVars
OwnerRename == /\ ownerActs > 0 /\ uploaded
               /\ ownerActs' = ownerActs - 1 /\ title' = title + 1
               /\ IF rendered THEN rendered' = FALSE /\ published' = FALSE /\ done' = FALSE
                              ELSE UNCHANGED <<rendered, published, done>>
               /\ UNCHANGED PhoneVars
               /\ UNCHANGED <<transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, summarized, noteRec, notified, tgMarker,
                              revision, step, lease, gpuWasBusy, vault, telegram, inbox, crashes, otherJobs>>
               /\ UNCHANGED EventVars
OwnerReprocess == /\ ownerActs > 0 /\ done
                  /\ ownerActs' = ownerActs - 1 /\ transcript' = "none" /\ summarized' = FALSE
                  /\ diarAttempted' = (IF FixDiarizeReset THEN FALSE ELSE diarAttempted) /\ UNCHANGED diarized
                  /\ rendered' = FALSE /\ published' = FALSE /\ notified' = FALSE /\ done' = FALSE
                  /\ revision' = revision + 1 /\ telegram' = 0
                  /\ UNCHANGED PhoneVars
                  /\ UNCHANGED <<hadWhisper, sawUpgrade, noteRec, tgMarker, title, step, lease, gpuWasBusy,
                                 vault, inbox, crashes, otherJobs>>
                  /\ UNCHANGED EventVars
Upgrade == /\ done /\ transcript = "device" /\ lease = "free"
           /\ transcript' = "whisper" /\ hadWhisper' = TRUE /\ sawUpgrade' = TRUE
           /\ diarAttempted' = (IF FixDiarizeReset THEN FALSE ELSE diarAttempted) /\ UNCHANGED diarized
           /\ summarized' = FALSE /\ rendered' = FALSE /\ published' = FALSE /\ done' = FALSE
           /\ revision' = revision + 1 /\ telegram' = 0 /\ notified' = TRUE
           /\ UNCHANGED PhoneVars
           /\ UNCHANGED <<noteRec, tgMarker, title, step, lease, gpuWasBusy, vault, inbox, crashes,
                          ownerActs, otherJobs>>
           /\ UNCHANGED EventVars

\* ---------------------------------------------------------------- the worker
WorkerCrash == /\ crashes > 0 /\ (Pending > 0 \/ step # "idle")
               /\ crashes' = crashes - 1 /\ step' = "idle"
               /\ lease' = IF lease = "worker" THEN "dead" ELSE lease
               /\ UNCHANGED PhoneVars
               /\ UNCHANGED <<transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, summarized, rendered, noteRec, published,
                              notified, tgMarker, done, revision, title, gpuWasBusy, vault, telegram, inbox,
                              ownerActs, otherJobs>>
               /\ UNCHANGED EventVars

Folder == event   \* Root, Course or Meeting
Keep(v) == UNCHANGED v
WorkerFinishStep ==
    /\ step # "idle"
    /\ step' = "idle"
    /\ CASE step = "whisper" -> /\ lease' = "free" /\ transcript' = "whisper" /\ hadWhisper' = TRUE
                                /\ diarAttempted' = (IF FixDiarizeReset THEN FALSE ELSE diarAttempted)
                                /\ UNCHANGED <<noteRec, published, notified, vault, oldNotes, event, eventSrc, linked>>
         [] step = "linking" -> \* the update closure: the owner may have chosen meanwhile
                                /\ linked' = TRUE
                                /\ IF ~FixLinkRecheck \/ eventSrc # "owner"
                                      THEN /\ eventSrc' = "auto"   \* no event known yet: link-pending
                                           /\ event' = (IF calendarHas THEN Course ELSE event)
                                      ELSE UNCHANGED <<event, eventSrc>>
                                /\ UNCHANGED <<lease, transcript, hadWhisper, noteRec, published, notified, vault, oldNotes,
                                               diarAttempted>>
         [] step = "pushed"  -> /\ noteRec' = (IF noteRec = NoNote THEN <<Folder, title>> ELSE noteRec)
                                /\ vault' = vault \ oldNotes /\ oldNotes' = {} /\ published' = TRUE
                                /\ UNCHANGED <<lease, transcript, hadWhisper, notified, event, eventSrc, linked,
                                               diarAttempted>>
         [] step = "sent"    -> /\ notified' = TRUE
                                /\ UNCHANGED <<lease, transcript, hadWhisper, noteRec, published, vault, oldNotes,
                                               event, eventSrc, linked, diarAttempted>>
    /\ UNCHANGED PhoneVars
    /\ UNCHANGED <<diarized, sawUpgrade, summarized, rendered, tgMarker, done, revision, title, gpuWasBusy,
                   telegram, inbox, crashes, ownerActs, otherJobs, ownerChoice, failed, eventActs, giveUps,
                   calendarHas, relinked>>

Name == IF noteRec = NoNote THEN title ELSE noteRec[2]   \* K7: the name stays
Path == <<Folder, Name>>
\* obsidian::move_note: the old path joins the copies to delete; the new one never does.
MovedOld == (IF noteRec = NoNote \/ noteRec = Path THEN oldNotes
             ELSE IF FixOldNotes THEN oldNotes \cup {noteRec}
             ELSE IF oldNotes = {} THEN {noteRec} ELSE oldNotes) \ {Path}
TgSend == ~FixTelegramMarker \/ tgMarker # revision

WorkerStartStep ==
    /\ step = "idle" /\ Pending > 0
    /\ UNCHANGED PhoneVars
    /\ UNCHANGED <<crashes, ownerActs, otherJobs, title, revision, sawUpgrade>>
    /\ UNCHANGED <<event, eventSrc, linked, ownerChoice, failed, eventActs, giveUps, calendarHas, relinked>>
    /\ (Pending # 4 => UNCHANGED oldNotes)
    /\ (Pending = 11 \/ (Pending = 1 /\ lease # "free") \/ UNCHANGED <<diarized, diarAttempted>>)
    /\ CASE Pending = 1 ->
              IF lease = "free"
                 THEN /\ lease' = "worker" /\ step' = "whisper"
                      /\ UNCHANGED <<transcript, hadWhisper, summarized, rendered, noteRec, published,
                                     notified, tgMarker, done, gpuWasBusy, vault, telegram, inbox>>
                 ELSE /\ devSent /\ ~(hadWhisper /\ FixNoDowngrade)   \* else: wait (no step)
                      /\ transcript' = "device" /\ gpuWasBusy' = TRUE
                      /\ diarAttempted' = (IF FixDiarizeReset THEN FALSE ELSE diarAttempted) /\ UNCHANGED diarized
                      /\ UNCHANGED <<hadWhisper, summarized, rendered, noteRec, published, notified,
                                     tgMarker, done, step, lease, vault, telegram, inbox>>
         [] Pending = 10 -> /\ step' = "linking"
                            /\ UNCHANGED <<transcript, hadWhisper, summarized, rendered, noteRec, published, notified,
                                           tgMarker, done, lease, gpuWasBusy, vault, telegram, inbox>>
         [] Pending = 11 -> /\ diarAttempted' = TRUE /\ diarized' = transcript
                            /\ UNCHANGED <<transcript, hadWhisper, summarized, rendered, noteRec, published, notified,
                                           tgMarker, done, step, lease, gpuWasBusy, vault, telegram, inbox>>
         [] Pending = 2 -> /\ summarized' = TRUE
                           /\ UNCHANGED <<transcript, hadWhisper, rendered, noteRec, published, notified,
                                          tgMarker, done, step, lease, gpuWasBusy, vault, telegram, inbox>>
         [] Pending = 3 -> /\ rendered' = TRUE
                           /\ UNCHANGED <<transcript, hadWhisper, summarized, noteRec, published, notified,
                                          tgMarker, done, step, lease, gpuWasBusy, vault, telegram, inbox>>
         [] Pending = 4 -> /\ IF FixNotePath /\ noteRec # Path
                                 THEN noteRec' = Path /\ oldNotes' = MovedOld
                                 ELSE UNCHANGED <<noteRec, oldNotes>>
                           /\ vault' = vault \cup {Path} /\ step' = "pushed"
                           /\ UNCHANGED <<transcript, hadWhisper, summarized, rendered, published, notified,
                                          tgMarker, done, lease, gpuWasBusy, telegram, inbox>>
         [] Pending = 5 -> /\ tgMarker' = (IF FixTelegramMarker THEN revision ELSE tgMarker)
                           /\ telegram' = (IF TgSend THEN telegram + 1 ELSE telegram)
                           /\ inbox' = IF \E i \in 1..Len(inbox) : inbox[i] = revision THEN inbox
                                       ELSE Append(inbox, revision)
                           /\ step' = "sent"
                           /\ UNCHANGED <<transcript, hadWhisper, summarized, rendered, noteRec, published,
                                          notified, done, lease, gpuWasBusy, vault>>
         [] Pending = 6 -> /\ done' = TRUE
                           /\ UNCHANGED <<transcript, hadWhisper, summarized, rendered, noteRec, published,
                                          notified, tgMarker, step, lease, gpuWasBusy, vault, telegram, inbox>>

\* ---------------------------------------------------------------- the calendar link
\* PUT /v1/memos/{id}/event: before the summary, once finished or failed (409 in between).
OwnerChoosesEvent ==
    /\ Events /\ eventActs > 0 /\ uploaded /\ (done \/ failed \/ ~summarized)
    /\ LET choice == IF ownerChoice = Meeting THEN Root ELSE Meeting IN
         /\ event' = choice /\ ownerChoice' = choice
    /\ eventSrc' = "owner" /\ linked' = TRUE /\ failed' = FALSE /\ eventActs' = eventActs - 1
    /\ IF summarized
          THEN /\ summarized' = FALSE /\ rendered' = FALSE /\ published' = FALSE /\ notified' = FALSE
               /\ done' = FALSE /\ revision' = revision + 1 /\ telegram' = 0
          ELSE UNCHANGED <<summarized, rendered, published, notified, done, revision, telegram>>
    /\ UNCHANGED PhoneVars
    /\ UNCHANGED <<transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, noteRec, tgMarker, title, step, lease, gpuWasBusy,
                   vault, inbox, crashes, ownerActs, otherJobs, oldNotes, giveUps, calendarHas, relinked>>
\* The last attempt failed (e.g. the old note's delete kept failing): `failed` until the owner acts.
WorkerGivesUp ==
    /\ giveUps > 0 /\ step = "idle" /\ Pending > 0
    /\ giveUps' = giveUps - 1 /\ failed' = TRUE
    /\ UNCHANGED PhoneVars /\ UNCHANGED ServerVars
    /\ UNCHANGED <<oldNotes, event, eventSrc, linked, ownerChoice, eventActs, calendarHas, relinked>>
\* The event becomes known to the server (lucina-memo's calendar cache), however late.
CalendarGainsEvent ==
    /\ Events /\ ~calendarHas /\ calendarHas' = TRUE
    /\ UNCHANGED PhoneVars /\ UNCHANGED ServerVars
    /\ UNCHANGED <<oldNotes, event, eventSrc, linked, ownerChoice, failed, eventActs, giveUps, relinked>>
\* worker::relink_one: one store.update links a finished (or failed) memo still without an event,
\* unless the owner chose (K9), and queues a quiet re-summary (the owner is not notified again).
WorkerRelink ==
    /\ FixRelink /\ calendarHas /\ linked /\ event = Root /\ eventSrc # "owner"
    /\ (done \/ failed) /\ step = "idle"
    /\ event' = Course /\ eventSrc' = "auto" /\ relinked' = TRUE /\ failed' = FALSE
    /\ summarized' = FALSE /\ rendered' = FALSE /\ published' = FALSE /\ done' = FALSE
    /\ revision' = revision + 1 /\ telegram' = 0
    /\ UNCHANGED PhoneVars
    /\ UNCHANGED <<transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, noteRec, notified, tgMarker, title, step, lease, gpuWasBusy,
                   vault, inbox, crashes, ownerActs, otherJobs, oldNotes, linked, ownerChoice, eventActs, giveUps,
                   calendarHas>>

Next == \/ TogglePause \/ Stop \/ KillApp \/ Relaunch \/ UploadOk \/ UploadFails \/ SendDeviceTranscript
        \/ OtherTakesGpu \/ OtherReleasesGpu \/ PruneDeadLease \/ OwnerRename \/ OwnerReprocess \/ Upgrade
        \/ WorkerCrash \/ WorkerFinishStep \/ WorkerStartStep \/ OwnerChoosesEvent \/ WorkerGivesUp
        \/ CalendarGainsEvent \/ WorkerRelink

Spec == Init /\ [][Next]_vars

\* ---------------------------------------------------------------- invariants
\* Every note in the vault is the memo's current one or a copy listed for deletion.
NoDuplicateNotes == \A p \in vault : p = noteRec \/ p \in oldNotes
OwnerChoiceKept == ownerChoice # None => (event = ownerChoice /\ eventSrc = "owner")
\* Witness target: a late event re-links the memo quietly (one notification, note in the course folder).
NeverRelinkedQuietly == ~(relinked /\ done /\ vault = {<<Course, 0>>} /\ Len(inbox) = 1)
TelegramAtMostOnce == telegram <= 1
AppInboxOncePerRevision == \A i, j \in 1..Len(inbox) : i # j => inbox[i] # inbox[j]
WhisperNeverDowngraded == ~(hadWhisper /\ transcript = "device")
NoLostAudio == rec # "lost"
LiveActivityMatchesRecorder == appAlive => CASE rec = "recording" -> la = "running"
                                             [] rec = "paused"    -> la = "paused"
                                             [] OTHER             -> la = "none"
DoneHasEveryOutput == done => transcript # "none" /\ summarized /\ rendered /\ published /\ notified
DoneLabelsFollowTranscript == done => diarized = transcript

\* Witness targets (a MUST FAIL cfg shows the state is reached).
NeverGpuBusyThenFree == ~(gpuWasBusy /\ transcript = "whisper" /\ done)
NeverDeviceReplaced == ~(sawUpgrade /\ done)
=============================================================================
