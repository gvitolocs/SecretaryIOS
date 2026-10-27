/// <reference lib="webworker" />
// LucinaDesk's video off the page's main thread: the desk sockets, their decoders and the drawing onto
// the player's canvas (an OffscreenCanvas) live here, so a busy page (Lucina's 3D stage, a long render)
// cannot stall the stream the way it would on the main thread. The page (lib/deskPipe.ts) keeps the
// tickets, the text messages and the input; it sends input through here.
//
// Page → worker: {type:"canvas", canvas} | {type:"open", id, url, protocols} | {type:"send", id, text}
//   | {type:"close", id} | {type:"current", id}
// Worker → page: {type:"ready", webcodecs} | {type:"open", id} | {type:"text", id, data}
//   | {type:"frame", id, width, height, codec, bytes} | {type:"closed", id, code, reason} | {type:"unsupported", id, codec}

import { DeskVideo } from "./deskVideo";
import type { ClientMessage } from "./deskWire";

declare const self: DedicatedWorkerGlobalScope;

let ctx: OffscreenCanvasRenderingContext2D | null = null;
let canvas: OffscreenCanvas | null = null;
/** Only this socket draws: during a takeover the old and the new socket both stream for a moment. */
let current = 0;
const socks = new Map<number, { ws: WebSocket; video: DeskVideo }>();

const post = (m: unknown) => self.postMessage(m);

function draw(frame: VideoFrame) {
  if (!canvas) return;
  if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
    canvas.width = frame.displayWidth;
    canvas.height = frame.displayHeight;
    ctx = null;
  }
  ctx ??= canvas.getContext("2d", { alpha: false, desynchronized: true }) as OffscreenCanvasRenderingContext2D | null;
  ctx?.drawImage(frame, 0, 0);
}

function open(id: number, url: string, protocols: string[]) {
  let ws: WebSocket;
  try {
    ws = new WebSocket(url, protocols);
  } catch {
    post({ type: "closed", id, code: 1006, reason: "" });
    return;
  }
  ws.binaryType = "arraybuffer";
  const send = (m: ClientMessage) => {
    if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(m));
  };
  const video = new DeskVideo({
    send,
    output: (frame) => {
      if (id === current) draw(frame);
      post({ type: "frame", id, width: frame.displayWidth, height: frame.displayHeight, codec: video.codec, bytes: video.bytes });
    },
    unsupported: (codec) => post({ type: "unsupported", id, codec }),
  });
  ws.onopen = () => post({ type: "open", id });
  ws.onmessage = (e) => {
    if (e.data instanceof ArrayBuffer) video.push(e.data);
    else if (typeof e.data === "string") post({ type: "text", id, data: e.data });
  };
  ws.onclose = (e) => {
    video.close();
    socks.delete(id);
    post({ type: "closed", id, code: e.code, reason: e.reason });
  };
  socks.set(id, { ws, video });
}

self.onmessage = (e: MessageEvent) => {
  const m = e.data as { type: string; id?: number; url?: string; protocols?: string[]; text?: string; canvas?: OffscreenCanvas };
  switch (m.type) {
    case "canvas":
      canvas = m.canvas ?? null;
      ctx = null;
      break;
    case "open":
      open(m.id!, m.url!, m.protocols!);
      break;
    case "send": {
      const s = socks.get(m.id!);
      if (s?.ws.readyState === WebSocket.OPEN) s.ws.send(m.text!);
      break;
    }
    case "close": {
      const s = socks.get(m.id!);
      if (!s) break;
      socks.delete(m.id!);
      s.ws.onclose = null;
      s.ws.onmessage = null;
      s.video.close();
      try {
        s.ws.close(1000);
      } catch {
        // gone
      }
      break;
    }
    case "current":
      current = m.id!;
      break;
  }
};

post({ type: "ready", webcodecs: typeof VideoDecoder === "function" && typeof EncodedVideoChunk === "function" });
