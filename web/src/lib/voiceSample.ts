// Records the short Voice Learn sample with the phone's (or laptop's) microphone: at most 10 seconds, then it is
// decoded to a 16 kHz WAV (lib/wav.ts) for POST /v1/voice/learn/sample. The audio only ever lives in this tab's
// memory and in that one request; nothing is stored, here or on the server.

import { createSignal } from "solid-js";
import { MAX_SAMPLE_SECONDS } from "../api/voiceLearn";
import { toWav16k } from "./wav";

export type SampleState = "idle" | "recording" | "processing";
export type SampleFailure = "unsupported" | "denied" | "failed";

export function createSampleRecorder(onClip: (wav: Blob) => void, onFailure: (why: SampleFailure, detail?: string) => void) {
  const [state, setState] = createSignal<SampleState>("idle");
  const [seconds, setSeconds] = createSignal(0);
  let rec: MediaRecorder | null = null;
  let stream: MediaStream | null = null;
  let chunks: Blob[] = [];
  let ticker = 0;
  let cap = 0;
  let discard = false;

  const mimeType = () => ["audio/mp4", "audio/webm;codecs=opus", "audio/webm", "audio/ogg;codecs=opus"].find((t) => MediaRecorder.isTypeSupported?.(t)) ?? "";

  function release() {
    window.clearInterval(ticker);
    window.clearTimeout(cap);
    stream?.getTracks().forEach((t) => t.stop());
    stream = null;
  }

  async function start() {
    if (state() !== "idle") return;
    if (typeof MediaRecorder === "undefined" || !navigator.mediaDevices?.getUserMedia) {
      onFailure("unsupported");
      return;
    }
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: false, noiseSuppression: false } });
    } catch {
      onFailure("denied");
      return;
    }
    const type = mimeType();
    rec = new MediaRecorder(stream, type ? { mimeType: type } : undefined);
    chunks = [];
    discard = false;
    rec.ondataavailable = (e) => e.data.size && chunks.push(e.data);
    rec.onstop = () => void finished();
    rec.start();
    const t0 = performance.now();
    setSeconds(0);
    setState("recording");
    ticker = window.setInterval(() => setSeconds(Math.min(MAX_SAMPLE_SECONDS, (performance.now() - t0) / 1000)), 200);
    cap = window.setTimeout(stop, MAX_SAMPLE_SECONDS * 1000);
  }

  function stop() {
    if (rec && rec.state !== "inactive") rec.stop();
  }

  /** Stop and throw the recording away (the page was left). */
  function cancel() {
    discard = true;
    stop();
    release();
    setState("idle");
  }

  async function finished() {
    release();
    const type = (rec?.mimeType || mimeType() || "audio/webm").split(";")[0] ?? "audio/webm";
    rec = null;
    if (discard) return;
    const clip = new Blob(chunks, { type });
    chunks = [];
    setState("processing");
    try {
      onClip(await toWav16k(clip));
    } catch (e) {
      onFailure("failed", e instanceof Error ? e.message : undefined);
    } finally {
      setState("idle");
    }
  }

  return { state, seconds, start, stop, cancel };
}
