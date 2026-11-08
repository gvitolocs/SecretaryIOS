// The live side of Lucina Prompter: the microphone at 16 kHz into lucina-memo's socket
// (wss /v1/memos/prompter/live, behind p1's @memos gate), transcript and verdicts back.
// Audio goes only to nezopt and is not stored; the protocol is in
// crates/lucina-memo/src/prompter/session.rs.
//
// Latency: every 100 ms chunk's send time is kept by its end sample, so a verdict about
// audio up to `audioMs` shows how long after that speech the point was crossed out.

import { openMicrophone as openMic, SAMPLE_RATE, type Microphone } from "./micCapture";
import type { Candidate, Verdict } from "./prompterTake";

// The microphone capture (16 kHz s16le worklet) is shared with the voice socket: micCapture.ts.
export const PROMPTER_CHUNK_MS = 100;
export { SAMPLE_RATE, type Microphone };

/** Open the microphone; `onChunk` gets 100 ms of 16 kHz PCM and its level (0..1). */
export function openMicrophone(onChunk: (pcm: ArrayBuffer, level: number) => void): Promise<Microphone> {
  return openMic(onChunk, { chunkMs: PROMPTER_CHUNK_MS });
}

export interface VerdictsMessage {
  type: "verdicts";
  phase: "local" | "jev";
  verdicts: (Verdict & { local?: number; jev?: number })[];
  audioMs: number;
  sttMs: number;
  judgeMs: number;
  jev: {
    asked: number;
    ms: number | null;
    inputTokens: number;
    error: string | null;
  };
  jevCalls: number;
  jevFailed: number;
}

export type LiveEvent =
  | { type: "status"; state: "loading" | "ready"; engine?: "gpu" | "cpu" }
  | {
      type: "transcript";
      final: string | null;
      partial: string;
      startMs: number;
      audioMs: number;
      sttMs: number;
      language: string;
    }
  | (VerdictsMessage & { latencyMs: number | null })
  | { type: "judge"; error: string; jevFailed: number }
  | { type: "error"; message: string }
  | { type: "closed"; clean: boolean };

export interface LiveOptions {
  language: string;
  vocabulary: string[];
  useJev: boolean;
  onEvent: (e: LiveEvent) => void;
  /** Tests: another socket. */
  socket?: (url: string) => WebSocket;
}

export interface LiveConnection {
  /** Send 100 ms of PCM (dropped while paused or closed). */
  audio(pcm: ArrayBuffer): void;
  points(section: string, candidates: Candidate[]): void;
  /** Audio received so far in ms (what the server counts as audioMs). */
  sentMs(): number;
  setPaused(p: boolean): void;
  close(): void;
}

export function liveUrl(loc: Pick<Location, "protocol" | "host"> = location): string {
  return `${loc.protocol === "https:" ? "wss" : "ws"}://${loc.host}/v1/memos/prompter/live`;
}

/** Latency of the speech up to `audioMs`, from the send times of the chunks. */
export function latencyFor(sent: readonly { endMs: number; at: number }[], audioMs: number, now: number): number | null {
  let lo = 0;
  let hi = sent.length - 1;
  let hit = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (sent[mid]!.endMs >= audioMs) {
      hit = mid;
      hi = mid - 1;
    } else lo = mid + 1;
  }
  return hit < 0 ? null : Math.max(0, Math.round(now - sent[hit]!.at));
}

export function connectLive(opts: LiveOptions): LiveConnection {
  const ws = (opts.socket ?? ((u) => new WebSocket(u)))(liveUrl());
  ws.binaryType = "arraybuffer";
  const sent: { endMs: number; at: number }[] = [];
  let samples = 0;
  let paused = false;
  let closed = false;
  let pendingPoints: string | null = null;
  const sendJson = (v: unknown) => {
    const s = JSON.stringify(v);
    if (ws.readyState === WebSocket.OPEN) ws.send(s);
    else if ((v as { type: string }).type === "points") pendingPoints = s;
  };
  ws.onopen = () => {
    ws.send(
      JSON.stringify({
        type: "start",
        language: opts.language,
        vocabulary: opts.vocabulary,
        useJev: opts.useJev,
      }),
    );
    if (pendingPoints) ws.send(pendingPoints);
    pendingPoints = null;
  };
  ws.onmessage = (m) => {
    if (typeof m.data !== "string") return;
    let v: Record<string, unknown>;
    try {
      v = JSON.parse(m.data) as Record<string, unknown>;
    } catch {
      return;
    }
    if (v.type === "verdicts") {
      const msg = v as unknown as VerdictsMessage;
      opts.onEvent({
        ...msg,
        latencyMs: latencyFor(sent, msg.audioMs, performance.now()),
      });
    } else if (v.type) opts.onEvent(v as unknown as LiveEvent);
  };
  ws.onerror = () => {
    if (!closed) opts.onEvent({ type: "error", message: "connection" });
  };
  ws.onclose = (e) => {
    const wasClosed = closed;
    closed = true;
    opts.onEvent({ type: "closed", clean: wasClosed || e.wasClean });
  };
  return {
    audio(pcm) {
      if (paused || closed || ws.readyState !== WebSocket.OPEN) return;
      ws.send(pcm);
      samples += pcm.byteLength / 2;
      sent.push({
        endMs: Math.round((samples * 1000) / SAMPLE_RATE),
        at: performance.now(),
      });
      if (sent.length > 3000) sent.splice(0, 1000);
    },
    points(section, candidates) {
      sendJson({ type: "points", section, candidates });
    },
    sentMs: () => Math.round((samples * 1000) / SAMPLE_RATE),
    setPaused(p) {
      // Close the open sentence: the server makes a final at a pause.
      if (p && !paused) this.audio(new ArrayBuffer((SAMPLE_RATE * 0.8 * 2) | 0));
      paused = p;
    },
    close() {
      if (closed) return;
      closed = true;
      try {
        if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: "stop" }));
      } finally {
        ws.close();
      }
    },
  };
}
