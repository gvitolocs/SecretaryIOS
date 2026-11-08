// A voice note, recorded and streamed: `start {purpose:"note"}` on press, 16 kHz s16le frames from the
// microphone while held, `end` on release (docs/lucinavoice-protocol.md 10.4). The UI part is
// components/VoiceNoteButton.tsx; this is the logic, with the socket and microphone injected for tests.

import { openMicrophone as openMic, type Microphone } from "./micCapture";
import { inputMs, type VoiceTurnEvent } from "./voiceProtocol";
import type { VoiceSocket } from "./voiceSocket";

/** The mic delivers a frame this often (protocol: 20 to 100 ms). */
export const NOTE_CHUNK_MS = 60;
/** Shorter than this is a mis-tap: nothing is sent. */
export const MIN_NOTE_MS = 300;
/** A press held at least this long is "hold to record"; a shorter one toggles recording on. */
export const HOLD_MS = 350;

export type NoteFailure = "offline" | "busy" | "oldServer" | "micBlocked" | "failed" | "tooShort" | "noSpeech";

/** `hold`: the release ends it. `toggle`: it goes on until the next press. */
export function pressKind(downAt: number, upAt: number): "hold" | "toggle" {
  return upAt - downAt >= HOLD_MS ? "hold" : "toggle";
}

export function failureOf(code: string): NoteFailure {
  if (code === "no_speech") return "noSpeech";
  if (code === "busy" || code === "rate_limited") return "busy";
  return "failed";
}

export interface NoteDeps {
  socket: Pick<VoiceSocket, "startTurn" | "oldServer" | "busy" | "status">;
  openMicrophone?: typeof openMic;
}

export interface NoteParams {
  sessionId?: string;
  replyTo?: string;
  lang?: string;
}

export interface NoteCallbacks {
  /** Audio sent so far, and the level of the last frame (0..1). */
  onProgress(ms: number, level: number): void;
  onTranscript(text: string): void;
  onNote(n: { shareId: string; userItemId: string }): void;
  /** Lucina answered (`done`). */
  onSettled(): void;
  /** Recording is over for any reason (released, cancelled, failed, capped). Once. */
  onStopped(): void;
  onFailure(why: NoteFailure): void;
  /** The 10 minute cap was reached: the note was sent as if released. */
  onCapped(): void;
}

export interface NoteRecording {
  /** Released: send what was said. */
  finish(): void;
  /** Dropped: nothing is posted. */
  cancel(): void;
  readonly ms: number;
}

/** Start recording a note. A string is why it could not start. */
export function recordNote(deps: NoteDeps, params: NoteParams, cb: NoteCallbacks): NoteRecording | NoteFailure {
  if (deps.socket.oldServer) return "oldServer";
  if (deps.socket.status !== "open") return "offline";
  let active = true;
  let ms = 0;
  let mic: Microphone | null = null;
  const stop = () => {
    if (!active) return;
    active = false;
    mic?.stop();
    mic = null;
    cb.onStopped();
  };
  const turn = deps.socket.startTurn(
    {
      mode: "ptt",
      purpose: "note",
      thread: "main",
      source: "web-chat",
      ...(params.sessionId ? { sessionId: params.sessionId } : {}),
      ...(params.replyTo ? { replyTo: params.replyTo } : {}),
      ...(params.lang ? { lang: params.lang } : {}),
    },
    (e: VoiceTurnEvent) => {
      switch (e.type) {
        case "transcript":
          cb.onTranscript(e.text);
          break;
        case "note":
          cb.onNote({ shareId: e.shareId, userItemId: e.userItemId });
          break;
        case "done":
          cb.onSettled();
          break;
        case "error":
          stop();
          cb.onFailure(failureOf(e.code));
          break;
        case "unsupported":
          stop();
          cb.onFailure("oldServer");
          break;
        case "closed":
          if (active) {
            stop();
            cb.onFailure("offline");
          }
          break;
        case "capped":
          stop();
          cb.onCapped();
          break;
        default:
          break;
      }
    },
  );
  if (!turn) return deps.socket.busy ? "busy" : "offline";
  (deps.openMicrophone ?? openMic)(
    (pcm, level) => {
      if (!active) return;
      turn.audio(pcm);
      ms += inputMs(pcm.byteLength);
      cb.onProgress(ms, level);
    },
    { chunkMs: NOTE_CHUNK_MS },
  ).then(
    (m) => {
      if (active) mic = m;
      else m.stop();
    },
    () => {
      if (!active) return;
      stop();
      turn.cancel();
      cb.onFailure("micBlocked");
    },
  );
  return {
    get ms() {
      return ms;
    },
    finish() {
      if (!active) return;
      stop();
      if (ms < MIN_NOTE_MS) {
        turn.cancel();
        cb.onFailure("tooShort");
      } else turn.end();
    },
    cancel() {
      if (!active) return;
      stop();
      turn.cancel();
    },
  };
}
