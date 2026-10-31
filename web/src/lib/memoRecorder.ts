// Records a voice memo in the browser (MediaRecorder: AAC/MP4 on Safari, Opus/WebM
// elsewhere), with a live level meter, pause and resume. Finished recordings go into an
// IndexedDB queue and upload with the resumable protocol, so a reload or a lost connection
// loses nothing: the queue resumes from the server's offset when the page is back online.

import { createSignal } from "solid-js";
import { memosApi, sha256Hex } from "../api/memos";
import { toApiError } from "../api/client";

export type RecState = "idle" | "recording" | "paused";

export interface QueuedMemo {
  clientId: string;
  recordedAt: string;
  durationSec: number;
  mime: string;
  sizeBytes: number;
  serverId?: string;
  uploaded: number;
  error?: string;
}

const DB = "secretary-memos";
const STORE = "queue";
const CHUNK = 4 << 20;

function db(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB, 1);
    req.onupgradeneeded = () => req.result.createObjectStore(STORE);
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

async function tx<T>(mode: IDBTransactionMode, f: (s: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  const d = await db();
  return new Promise((resolve, reject) => {
    const r = f(d.transaction(STORE, mode).objectStore(STORE));
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
}

interface Entry {
  meta: QueuedMemo;
  blob: Blob;
}

const [queue, setQueue] = createSignal<QueuedMemo[]>([]);
export { queue };

async function loadQueue(): Promise<Entry[]> {
  try {
    return ((await tx("readonly", (s) => s.getAll())) as Entry[]) ?? [];
  } catch {
    return [];
  }
}

async function refreshQueue() {
  setQueue((await loadQueue()).map((e) => e.meta));
}

async function save(e: Entry) {
  await tx("readwrite", (s) => s.put(e, e.meta.clientId));
  await refreshQueue();
}

async function drop(clientId: string) {
  await tx("readwrite", (s) => s.delete(clientId));
  await refreshQueue();
}

let uploading = false;
let onUploaded: ((id: string) => void) | null = null;
export function setOnUploaded(f: ((id: string) => void) | null) {
  onUploaded = f;
}

/** Uploads every queued memo, one at a time. Safe to call often. */
export async function flushQueue(): Promise<void> {
  if (uploading || !navigator.onLine) return;
  uploading = true;
  try {
    for (const entry of await loadQueue()) {
      const m = entry.meta;
      try {
        let id = m.serverId;
        let offset = 0;
        if (!id) {
          const created = await memosApi.create({
            clientId: m.clientId,
            recordedAt: m.recordedAt,
            timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
            durationSec: m.durationSec,
            mime: m.mime,
            sizeBytes: m.sizeBytes,
            source: "web",
          });
          id = created.id;
          offset = created.receivedBytes;
          m.serverId = id;
          if (created.status !== "uploading") {
            await drop(m.clientId);
            onUploaded?.(id);
            continue;
          }
        } else {
          const d = await memosApi.detail(id);
          offset = d.memo.receivedBytes;
          if (d.memo.status !== "uploading") {
            await drop(m.clientId);
            onUploaded?.(id);
            continue;
          }
        }
        while (offset < m.sizeBytes) {
          const next = await memosApi.upload(id, offset, entry.blob.slice(offset, offset + CHUNK));
          if (next <= offset) throw new Error("The upload is not moving forward.");
          offset = next;
          m.uploaded = offset;
          m.error = undefined;
          await save(entry);
        }
        await memosApi.complete(id, await sha256Hex(entry.blob));
        await drop(m.clientId);
        onUploaded?.(id);
      } catch (e) {
        m.error = toApiError(e).message;
        await save(entry);
      }
    }
  } finally {
    uploading = false;
  }
}

if (typeof window !== "undefined") {
  window.addEventListener("online", () => void flushQueue());
  void refreshQueue();
}

/** The microphone side: one recording at a time. */
export function createMemoRecorder() {
  const [state, setState] = createSignal<RecState>("idle");
  const [elapsed, setElapsed] = createSignal(0);
  const [levels, setLevels] = createSignal<number[]>([]);
  const [error, setError] = createSignal<string | null>(null);

  let rec: MediaRecorder | null = null;
  let stream: MediaStream | null = null;
  let ctx: AudioContext | null = null;
  let analyser: AnalyserNode | null = null;
  let chunks: Blob[] = [];
  let startedAt = new Date();
  let runStart = 0;
  let accumulated = 0;
  let raf = 0;
  let discardNext = false;

  const mimeType = () =>
    ["audio/mp4", "audio/webm;codecs=opus", "audio/webm", "audio/ogg;codecs=opus"].find((t) => MediaRecorder.isTypeSupported?.(t)) ?? "";

  function tick() {
    if (analyser && state() === "recording") {
      const buf = new Uint8Array(analyser.fftSize);
      analyser.getByteTimeDomainData(buf);
      let sum = 0;
      for (const v of buf) sum += ((v - 128) / 128) ** 2;
      const rms = Math.sqrt(sum / buf.length);
      const level = Math.min(1, Math.pow(rms * 3.2, 0.8));
      setLevels((l) => [...l.slice(-159), level]);
      setElapsed(accumulated + (performance.now() - runStart) / 1000);
    }
    raf = requestAnimationFrame(tick);
  }

  async function start() {
    setError(null);
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true } });
    } catch {
      setError("mic");
      return;
    }
    const type = mimeType();
    rec = new MediaRecorder(stream, type ? { mimeType: type, audioBitsPerSecond: 96_000 } : undefined);
    chunks = [];
    rec.ondataavailable = (e) => e.data.size && chunks.push(e.data);
    rec.onstop = () => void finished();
    ctx = new AudioContext();
    analyser = ctx.createAnalyser();
    analyser.fftSize = 1024;
    ctx.createMediaStreamSource(stream).connect(analyser);
    startedAt = new Date();
    accumulated = 0;
    runStart = performance.now();
    setLevels([]);
    setElapsed(0);
    rec.start(1000);
    setState("recording");
    raf = requestAnimationFrame(tick);
  }

  function pause() {
    if (rec?.state !== "recording") return;
    rec.pause();
    accumulated += (performance.now() - runStart) / 1000;
    setElapsed(accumulated);
    setState("paused");
  }

  function resume() {
    if (rec?.state !== "paused") return;
    rec.resume();
    runStart = performance.now();
    setState("recording");
  }

  function stop(discard = false) {
    if (!rec || rec.state === "inactive") return;
    if (state() === "recording") accumulated += (performance.now() - runStart) / 1000;
    discardNext = discard;
    rec.stop();
  }

  async function finished() {
    cancelAnimationFrame(raf);
    stream?.getTracks().forEach((t) => t.stop());
    void ctx?.close();
    const type = (rec?.mimeType || mimeType() || "audio/webm").split(";")[0] ?? "audio/webm";
    rec = null;
    setState("idle");
    const blob = new Blob(chunks, { type });
    chunks = [];
    if (discardNext || blob.size === 0 || accumulated < 0.5) return;
    const meta: QueuedMemo = {
      clientId: crypto.randomUUID(),
      recordedAt: startedAt.toISOString(),
      durationSec: accumulated,
      mime: type,
      sizeBytes: blob.size,
      uploaded: 0,
    };
    await save({ meta, blob });
    void flushQueue();
  }

  return { state, elapsed, levels, error, start, pause, resume, stop };
}
