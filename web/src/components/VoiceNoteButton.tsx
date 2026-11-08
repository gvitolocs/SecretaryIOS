import { createSignal, onCleanup, Show } from "solid-js";
import "../styles/voicein.css";
import { locale, t } from "../i18n";
import { chatSessionId } from "../lib/ids";
import { formatClock, useVoiceSocket } from "../lib/useVoiceSocket";
import { MIN_NOTE_MS, pressKind, recordNote, type NoteFailure, type NoteRecording } from "../lib/voiceNote";
import { replyTarget } from "../state/chat";
import { addPendingNote, removePendingNote, updatePendingNote } from "../state/voiceNotes";
import { MicIcon } from "./Icons";

const FAILURE_KEYS: Record<NoteFailure, Parameters<typeof t>[0]> = {
  offline: "voiceIn.offline",
  busy: "voiceIn.busy",
  oldServer: "voiceIn.oldServer",
  micBlocked: "voiceIn.micBlocked",
  failed: "voiceIn.failed",
  tooShort: "voiceIn.tooShort",
  noSpeech: "voiceIn.noSpeech",
};
const TOAST_MS = 4_000;

/** The language tag the server's speech-to-text takes; other languages are left to it to detect. */
function serverLang(): string | undefined {
  const l = locale();
  return l === "it" || l === "en" ? l : undefined;
}

/**
 * The chat composer's voice-note button. Hold it to record and let go to send; or click once to start
 * and again to send. Esc or the cancel button drops the recording. The audio streams up the voice
 * socket as it is captured (lib/voiceNote.ts), so the server has it all the moment the owner stops.
 */
export function VoiceNoteButton() {
  const { socket, status } = useVoiceSocket();
  const [recording, setRecording] = createSignal(false);
  const [ms, setMs] = createSignal(0);
  const [level, setLevel] = createSignal(0);
  const [toast, setToast] = createSignal("");
  let rec: NoteRecording | null = null;
  let downAt = 0;
  /** This press began the recording (so its release decides hold or toggle). */
  let pressStarted = false;
  /** The pending bubble of the note being sent. */
  let pendingKey: string | null = null;
  let toastTimer: ReturnType<typeof setTimeout> | undefined;

  function say(text: string) {
    clearTimeout(toastTimer);
    setToast(text);
    if (text) toastTimer = setTimeout(() => setToast(""), TOAST_MS);
  }

  function begin(): boolean {
    say("");
    pendingKey = null;
    const started = recordNote(
      { socket },
      { sessionId: chatSessionId(), replyTo: replyTarget()?.id, lang: serverLang() },
      {
        onProgress: (len, lvl) => {
          setMs(len);
          setLevel(lvl);
        },
        onTranscript: (text) => {
          if (pendingKey) updatePendingNote(pendingKey, { transcript: text });
        },
        onNote: (n) => {
          if (pendingKey) updatePendingNote(pendingKey, { userItemId: n.userItemId });
        },
        onSettled: () => {},
        onStopped: () => {
          setRecording(false);
          setLevel(0);
          rec = null;
        },
        onFailure: (why) => {
          if (pendingKey) removePendingNote(pendingKey);
          pendingKey = null;
          say(t(FAILURE_KEYS[why]));
        },
        onCapped: () => {
          pendingKey = addPendingNote();
          say(t("voiceIn.capped"));
        },
      },
    );
    if (typeof started === "string") {
      say(t(FAILURE_KEYS[started]));
      return false;
    }
    rec = started;
    setMs(0);
    setRecording(true);
    return true;
  }

  function release() {
    const r = rec;
    if (!r) return;
    const long = r.ms >= MIN_NOTE_MS;
    r.finish();
    if (long) pendingKey = addPendingNote();
  }

  function cancel() {
    rec?.cancel();
  }

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    if (recording()) {
      // The second press of a click-to-toggle recording ends it.
      pressStarted = false;
      release();
      return;
    }
    downAt = performance.now();
    pressStarted = begin();
    if (pressStarted) (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
  }

  function onPointerUp() {
    if (!pressStarted) return;
    pressStarted = false;
    if (pressKind(downAt, performance.now()) === "hold") release();
  }

  function onPointerCancel() {
    if (!pressStarted) return;
    pressStarted = false;
    cancel();
  }

  /** Keyboard and assistive tech: a click with no pointer behind it toggles. */
  function onClick(e: MouseEvent) {
    if (e.detail !== 0) return;
    if (recording()) release();
    else begin();
  }

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape" && recording()) {
      e.preventDefault();
      cancel();
    }
  };
  document.addEventListener("keydown", onKey);
  onCleanup(() => {
    document.removeEventListener("keydown", onKey);
    clearTimeout(toastTimer);
    rec?.cancel();
  });

  return (
    <div class="voice-note">
      <button
        type="button"
        class="voice-note-btn"
        classList={{ "voice-note-btn--rec": recording(), "voice-note-btn--off": status() !== "open" && !recording() }}
        aria-label={t("voiceIn.record")}
        aria-pressed={recording() ? "true" : "false"}
        title={t("voiceIn.record")}
        onPointerDown={onPointerDown}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerCancel}
        onClick={onClick}
        onContextMenu={(e) => e.preventDefault()}
      >
        <MicIcon />
      </button>
      <Show when={recording()}>
        <div class="voice-bar" role="status">
          <span class="voice-bar-dot" aria-hidden="true" />
          <span class="voice-bar-label">{t("voiceIn.recording")}</span>
          <span class="voice-bar-time">{formatClock(ms())}</span>
          <span class="voice-bar-meter" aria-hidden="true">
            <span style={{ width: `${Math.min(100, Math.round(level() * 300))}%` }} />
          </span>
          <button type="button" class="voice-bar-cancel" onClick={cancel} aria-label={t("voiceIn.cancel")} title={t("voiceIn.cancel")}>
            ×
          </button>
        </div>
      </Show>
      <Show when={toast()}>
        <p class="voice-toast" role="alert">
          {toast()}
        </p>
      </Show>
    </div>
  );
}
