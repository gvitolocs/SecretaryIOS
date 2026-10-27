// One desk socket's video: the server's binary frames → a WebCodecs decoder → decoded frames, with the
// acks the host paces itself on. No DOM, no window: it runs on the page or in lib/deskWorker.ts.

import { parseVideo, WEBCODECS, type ClientMessage, type DeskCodec } from "./deskWire";

/** Frames waiting in the decoder before acks wait for it (see `ackAt`). */
const DECODE_BACKLOG = 3;
/** While waiting for a key frame, ask again at most this often (each ask makes the host encode one). */
const REFRESH_EVERY_MS = 300;

export interface DeskVideoHost {
  send(m: ClientMessage): void;
  /** A decoded frame; it is closed when this returns. */
  output(frame: VideoFrame): void;
  /** The host sends a codec this browser cannot decode. */
  unsupported(codec: DeskCodec): void;
}

export class DeskVideo {
  codec: DeskCodec | null = null;
  bytes = 0;
  private decoder: VideoDecoder | null = null;
  private waitingKey = true;
  private closed = false;
  private lastRefresh = -Infinity;
  /**
   * Timestamps (µs) of frames flagged "last" whose ack waits for the decoder. The host sends the next
   * batch only after an ack; acking once the frame is drawn would tie the stream to whatever thread
   * draws it. The ack goes as soon as the frame is queued, and waits for the decoder only while it is
   * behind (that is the backpressure the host needs).
   */
  private ackAt = new Set<number>();

  constructor(private readonly host: DeskVideoHost) {}

  /** One binary message from the socket. */
  push(buf: ArrayBuffer): void {
    if (this.closed) return;
    const f = parseVideo(buf);
    if (!f) return;
    this.bytes += f.data.byteLength;
    if (f.codec !== this.codec || !this.decoder) {
      if (!this.makeDecoder(f.codec)) return;
      this.waitingKey = true;
    }
    if (this.waitingKey && !f.key) {
      // Nothing to show until a key frame; keep the host going and ask for one.
      if (f.last) this.host.send({ type: "ack" });
      this.refresh();
      return;
    }
    this.waitingKey = false;
    const timestamp = f.pts * 1000;
    if (f.last) {
      if ((this.decoder!.decodeQueueSize ?? DECODE_BACKLOG) < DECODE_BACKLOG) this.host.send({ type: "ack" });
      else this.ackAt.add(timestamp);
    }
    try {
      this.decoder!.decode(new EncodedVideoChunk({ type: f.key ? "key" : "delta", timestamp, data: f.data }));
    } catch {
      this.recover();
    }
  }

  private makeDecoder(codec: DeskCodec): boolean {
    this.closeDecoder();
    this.decoder = new VideoDecoder({
      output: (frame) => {
        if (this.closed) {
          frame.close();
          return;
        }
        try {
          this.host.output(frame);
        } finally {
          frame.close();
        }
        if (this.ackAt.delete(frame.timestamp)) this.host.send({ type: "ack" });
      },
      error: (e) => {
        if (e?.name === "NotSupportedError") {
          this.host.unsupported(codec);
          return;
        }
        this.recover();
      },
    });
    try {
      this.decoder.configure({ codec: WEBCODECS[codec], optimizeForLatency: true, hardwareAcceleration: "no-preference" });
    } catch {
      this.host.unsupported(codec);
      return false;
    }
    this.codec = codec;
    return true;
  }

  /** A decoder error: start over at the next key frame, and let the host go on. */
  private recover() {
    this.codec = null;
    this.closeDecoder();
    this.waitingKey = true;
    if (this.ackAt.size) this.host.send({ type: "ack" });
    this.ackAt.clear();
    this.refresh(true);
  }

  /** Ask the host for a key frame (throttled unless `now`). The socket asks once right at login. */
  refresh(now = false): void {
    const t = performance.now();
    if (!now && t - this.lastRefresh < REFRESH_EVERY_MS) return;
    this.lastRefresh = t;
    this.host.send({ type: "refresh" });
  }

  private closeDecoder() {
    try {
      if (this.decoder && this.decoder.state !== "closed") this.decoder.close();
    } catch {
      // gone
    }
    this.decoder = null;
  }

  close(): void {
    this.closed = true;
    this.closeDecoder();
  }
}
