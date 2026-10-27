------------------------------- MODULE Memos -------------------------------
(***************************************************************************)
(* Voice memos end to end (docs/memos.md): the iPhone recorder and Live    *)
(* Activity, the upload queue, the lucina-memo worker (transcript, summary,*)
(* PDF, Obsidian push, notification), the GPU lease, owner rename and      *)
(* reprocess, iOS killing the app, worker crashes, speaker labels (K11). *)
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
          Kills, Crashes, NetFails, OwnerActs, OtherJobs

VARIABLES rec, appAlive, la, queued, uploaded, devSent,
          transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, summarized, rendered, noteRec, published,
          notified, tgMarker, done, revision, title, step, lease, gpuWasBusy,
          vault, telegram, inbox,
          kills, crashes, netFails, ownerActs, otherJobs, toggles

vars == <<rec, appAlive, la, queued, uploaded, devSent,
          transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, summarized, rendered, noteRec, published,
          notified, tgMarker, done, revision, title, step, lease, gpuWasBusy,
          vault, telegram, inbox,
          kills, crashes, netFails, ownerActs, otherJobs, toggles>>

None == -1   \* no transcript / no recorded note / no marker / no Live Activity

Init == /\ rec = "recording" /\ appAlive = TRUE /\ la = "running"
        /\ queued = FALSE /\ uploaded = FALSE /\ devSent = FALSE
        /\ transcript = "none" /\ diarized = "none" /\ diarAttempted = FALSE /\ hadWhisper = FALSE /\ sawUpgrade = FALSE
        /\ summarized = FALSE /\ rendered = FALSE /\ noteRec = None /\ published = FALSE
        /\ notified = FALSE /\ tgMarker = None /\ done = FALSE /\ revision = 0 /\ title = 0
        /\ step = "idle" /\ lease = "free" /\ gpuWasBusy = FALSE
        /\ vault = {} /\ telegram = 0 /\ inbox = <<>>
        /\ kills = Kills /\ crashes = Crashes /\ netFails = NetFails
        /\ ownerActs = OwnerActs /\ otherJobs = OtherJobs /\ toggles = 2

Pending == IF ~uploaded \/ done THEN 0
           ELSE IF transcript = "none" THEN 1
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
Stop == /\ appAlive /\ rec \in {"recording", "paused"}
        /\ rec' = "saved" /\ la' = "none" /\ queued' = TRUE
        /\ UNCHANGED <<appAlive, uploaded, devSent, kills, netFails, toggles>> /\ UNCHANGED ServerVars
KillApp == /\ appAlive /\ kills > 0 /\ rec # "lost"
           /\ kills' = kills - 1 /\ appAlive' = FALSE
           /\ rec' = IF rec \in {"recording", "paused"} THEN "killed" ELSE rec
           /\ UNCHANGED <<la, queued, uploaded, devSent, netFails, toggles>> /\ UNCHANGED ServerVars
Relaunch == /\ ~appAlive /\ appAlive' = TRUE /\ la' = "none"
            /\ IF rec = "killed"
                  THEN IF FixRecovery THEN rec' = "recovered" /\ queued' = TRUE
                                      ELSE rec' = "lost" /\ UNCHANGED queued
                  ELSE UNCHANGED <<rec, queued>>
            /\ UNCHANGED <<uploaded, devSent, kills, netFails, toggles>> /\ UNCHANGED ServerVars
UploadOk == /\ appAlive /\ queued /\ ~uploaded /\ uploaded' = TRUE
            /\ UNCHANGED <<rec, appAlive, la, queued, devSent, kills, netFails, toggles>> /\ UNCHANGED ServerVars
UploadFails == /\ appAlive /\ queued /\ ~uploaded /\ netFails > 0 /\ netFails' = netFails - 1
               /\ UNCHANGED <<rec, appAlive, la, queued, uploaded, devSent, kills, toggles>> /\ UNCHANGED ServerVars
SendDeviceTranscript == /\ appAlive /\ queued /\ uploaded /\ ~devSent /\ devSent' = TRUE
                        /\ UNCHANGED <<rec, appAlive, la, queued, uploaded, kills, netFails, toggles>>
                        /\ UNCHANGED ServerVars

PhoneVars == <<rec, appAlive, la, queued, uploaded, devSent, kills, netFails, toggles>>

\* ---------------------------------------------------------------- the GPU and the owner
OtherTakesGpu == /\ otherJobs > 0 /\ lease = "free" /\ otherJobs' = otherJobs - 1 /\ lease' = "other"
                 /\ UNCHANGED PhoneVars
                 /\ UNCHANGED <<transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, summarized, rendered, noteRec, published,
                                notified, tgMarker, done, revision, title, step, gpuWasBusy, vault,
                                telegram, inbox, crashes, ownerActs>>
OtherReleasesGpu == /\ lease = "other" /\ lease' = "free"
                    /\ UNCHANGED PhoneVars
                    /\ UNCHANGED <<transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, summarized, rendered, noteRec, published,
                                   notified, tgMarker, done, revision, title, step, gpuWasBusy, vault,
                                   telegram, inbox, crashes, ownerActs, otherJobs>>
PruneDeadLease == /\ PruneDead /\ lease = "dead" /\ lease' = "free"
                  /\ UNCHANGED PhoneVars
                  /\ UNCHANGED <<transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, summarized, rendered, noteRec, published,
                                 notified, tgMarker, done, revision, title, step, gpuWasBusy, vault,
                                 telegram, inbox, crashes, ownerActs, otherJobs>>
OwnerRename == /\ ownerActs > 0 /\ uploaded
               /\ ownerActs' = ownerActs - 1 /\ title' = title + 1
               /\ IF rendered THEN rendered' = FALSE /\ published' = FALSE /\ done' = FALSE
                              ELSE UNCHANGED <<rendered, published, done>>
               /\ UNCHANGED PhoneVars
               /\ UNCHANGED <<transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, summarized, noteRec, notified, tgMarker,
                              revision, step, lease, gpuWasBusy, vault, telegram, inbox, crashes, otherJobs>>
OwnerReprocess == /\ ownerActs > 0 /\ done
                  /\ ownerActs' = ownerActs - 1 /\ transcript' = "none" /\ summarized' = FALSE
                  /\ diarAttempted' = (IF FixDiarizeReset THEN FALSE ELSE diarAttempted) /\ UNCHANGED diarized
                  /\ rendered' = FALSE /\ published' = FALSE /\ notified' = FALSE /\ done' = FALSE
                  /\ revision' = revision + 1 /\ telegram' = 0
                  /\ UNCHANGED PhoneVars
                  /\ UNCHANGED <<hadWhisper, sawUpgrade, noteRec, tgMarker, title, step, lease, gpuWasBusy,
                                 vault, inbox, crashes, otherJobs>>
Upgrade == /\ done /\ transcript = "device" /\ lease = "free"
           /\ transcript' = "whisper" /\ hadWhisper' = TRUE /\ sawUpgrade' = TRUE
           /\ diarAttempted' = (IF FixDiarizeReset THEN FALSE ELSE diarAttempted) /\ UNCHANGED diarized
           /\ summarized' = FALSE /\ rendered' = FALSE /\ published' = FALSE /\ done' = FALSE
           /\ revision' = revision + 1 /\ telegram' = 0 /\ notified' = TRUE
           /\ UNCHANGED PhoneVars
           /\ UNCHANGED <<noteRec, tgMarker, title, step, lease, gpuWasBusy, vault, inbox, crashes,
                          ownerActs, otherJobs>>

\* ---------------------------------------------------------------- the worker
WorkerCrash == /\ crashes > 0 /\ (Pending > 0 \/ step # "idle")
               /\ crashes' = crashes - 1 /\ step' = "idle"
               /\ lease' = IF lease = "worker" THEN "dead" ELSE lease
               /\ UNCHANGED PhoneVars
               /\ UNCHANGED <<transcript, diarized, diarAttempted, hadWhisper, sawUpgrade, summarized, rendered, noteRec, published,
                              notified, tgMarker, done, revision, title, gpuWasBusy, vault, telegram, inbox,
                              ownerActs, otherJobs>>

Keep(v) == UNCHANGED v
WorkerFinishStep ==
    /\ step # "idle"
    /\ step' = "idle"
    /\ CASE step = "whisper" -> /\ lease' = "free" /\ transcript' = "whisper" /\ hadWhisper' = TRUE
                                /\ diarAttempted' = (IF FixDiarizeReset THEN FALSE ELSE diarAttempted)
                                /\ UNCHANGED <<noteRec, published, notified>>
         [] step = "pushed"  -> /\ noteRec' = (IF noteRec = None THEN title ELSE noteRec) /\ published' = TRUE
                                /\ UNCHANGED <<lease, transcript, hadWhisper, notified, diarAttempted>>
         [] step = "sent"    -> /\ notified' = TRUE
                                /\ UNCHANGED <<lease, transcript, hadWhisper, noteRec, published, diarAttempted>>
    /\ UNCHANGED PhoneVars
    /\ UNCHANGED <<diarized, sawUpgrade, summarized, rendered, tgMarker, done, revision, title, gpuWasBusy,
                   vault, telegram, inbox, crashes, ownerActs, otherJobs>>

Name == IF noteRec = None THEN title ELSE noteRec
TgSend == ~FixTelegramMarker \/ tgMarker # revision

WorkerStartStep ==
    /\ step = "idle" /\ Pending > 0
    /\ UNCHANGED PhoneVars
    /\ UNCHANGED <<crashes, ownerActs, otherJobs, title, revision, sawUpgrade>>
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
         [] Pending = 11 -> /\ diarAttempted' = TRUE /\ diarized' = transcript
                            /\ UNCHANGED <<transcript, hadWhisper, summarized, rendered, noteRec, published, notified,
                                           tgMarker, done, step, lease, gpuWasBusy, vault, telegram, inbox>>
         [] Pending = 2 -> /\ summarized' = TRUE
                           /\ UNCHANGED <<transcript, hadWhisper, rendered, noteRec, published, notified,
                                          tgMarker, done, step, lease, gpuWasBusy, vault, telegram, inbox>>
         [] Pending = 3 -> /\ rendered' = TRUE
                           /\ UNCHANGED <<transcript, hadWhisper, summarized, noteRec, published, notified,
                                          tgMarker, done, step, lease, gpuWasBusy, vault, telegram, inbox>>
         [] Pending = 4 -> /\ noteRec' = (IF FixNotePath THEN Name ELSE noteRec)
                           /\ vault' = vault \cup {Name} /\ step' = "pushed"
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

Next == \/ TogglePause \/ Stop \/ KillApp \/ Relaunch \/ UploadOk \/ UploadFails \/ SendDeviceTranscript
        \/ OtherTakesGpu \/ OtherReleasesGpu \/ PruneDeadLease \/ OwnerRename \/ OwnerReprocess \/ Upgrade
        \/ WorkerCrash \/ WorkerFinishStep \/ WorkerStartStep

Spec == Init /\ [][Next]_vars

\* ---------------------------------------------------------------- invariants
NoDuplicateNotes == Cardinality(vault) <= 1
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
