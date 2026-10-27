// The page's handle on lib/deskWorker.ts: one worker per player, which owns the player's canvas (handed
// over as an OffscreenCanvas) and every desk socket of that player. Where a browser cannot (no module
// workers, no OffscreenCanvas, no WebCodecs in workers) `createDeskPipe` resolves null and lib/deskClient
// does it all on the page instead.

import type { ClientMessage, DeskCodec } from "./deskWire";

export interface PipeEvents {
  open(): void;
  text(data: string): void;
  frame(info: { width: number; height: number; codec: DeskCodec | null; bytes: number }): void;
  closed(code: number, reason: string): void;
  unsupported(codec: DeskCodec): void;
  /** A binary frame of kind 2 (a clipboard image), whole. */
  image(data: ArrayBuffer): void;
}

const READY_TIMEOUT_MS = 2000;

export class DeskPipe {
  private nextId = 1;
  private readonly handlers = new Map<number, PipeEvents>();

  /** Use `createDeskPipe`. */
  constructor(private readonly worker: Worker) {
    worker.onmessage = (e: MessageEvent) => {
      const m = e.data as { type: string; id: number; data?: string | ArrayBuffer; code?: number; reason?: string; codec?: DeskCodec; width?: number; height?: number; bytes?: number };
      const h = this.handlers.get(m.id);
      if (!h) return;
      switch (m.type) {
        case "open":
          h.open();
          break;
        case "text":
          h.text(typeof m.data === "string" ? m.data : "");
          break;
        case "image":
          if (m.data instanceof ArrayBuffer) h.image(m.data);
          break;
        case "frame":
          h.frame({ width: m.width ?? 0, height: m.height ?? 0, codec: m.codec ?? null, bytes: m.bytes ?? 0 });
          break;
        case "closed":
          this.handlers.delete(m.id);
          h.closed(m.code ?? 1006, m.reason ?? "");
          break;
        case "unsupported":
          h.unsupported(m.codec!);
          break;
      }
    };
  }

  open(url: string, protocols: string[], events: PipeEvents): number {
    const id = this.nextId++;
    this.handlers.set(id, events);
    this.worker.postMessage({ type: "open", id, url, protocols });
    return id;
  }

  send(id: number, m: ClientMessage): void {
    this.worker.postMessage({ type: "send", id, text: JSON.stringify(m) });
  }

  /** A binary frame (a clipboard image); the buffer is handed over. */
  sendBinary(id: number, data: ArrayBuffer): void {
    this.worker.postMessage({ type: "sendBinary", id, data }, [data]);
  }

  close(id: number): void {
    this.handlers.delete(id);
    this.worker.postMessage({ type: "close", id });
  }

  /** Which socket draws on the canvas. */
  setCurrent(id: number): void {
    this.worker.postMessage({ type: "current", id });
  }

  dispose(): void {
    this.handlers.clear();
    this.worker.terminate();
  }
}

/**
 * A worker that can decode, then the canvas handed to it. Null when this browser cannot: the canvas
 * is untouched then, so the page can still draw on it.
 */
export async function createDeskPipe(canvas: HTMLCanvasElement): Promise<DeskPipe | null> {
  if (typeof Worker !== "function" || typeof OffscreenCanvas !== "function" || typeof canvas.transferControlToOffscreen !== "function") return null;
  let worker: Worker;
  try {
    worker = new Worker(new URL("./deskWorker.ts", import.meta.url), { type: "module", name: "lucinadesk" });
  } catch {
    return null;
  }
  const ok = await new Promise<boolean>((resolve) => {
    const timer = setTimeout(() => resolve(false), READY_TIMEOUT_MS);
    worker.onmessage = (e: MessageEvent) => {
      const m = e.data as { type?: string; webcodecs?: boolean };
      if (m?.type !== "ready") return;
      clearTimeout(timer);
      resolve(m.webcodecs === true);
    };
    worker.onerror = () => {
      clearTimeout(timer);
      resolve(false);
    };
  });
  if (!ok) {
    worker.terminate();
    return null;
  }
  try {
    const off = canvas.transferControlToOffscreen();
    worker.postMessage({ type: "canvas", canvas: off }, [off]);
  } catch {
    worker.terminate();
    return null;
  }
  worker.onerror = null;
  return new DeskPipe(worker);
}
