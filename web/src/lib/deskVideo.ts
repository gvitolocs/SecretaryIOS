// One desk socket's video: the server's binary frames → a WebCodecs decoder → decoded frames, with the
// acks the host paces itself on. No DOM, no window: it runs on the page or in lib/deskWorker.ts.

import { H264_FALLBACKS, parseVideo, WEBCODECS, type ClientMessage, type DeskCodec } from "./deskWire";

/** Frames waiting in the decoder before acks wait for it (see `ackAt`). */
const DECODE_BACKLOG = 3;

export interface DeskVideoHost {
  send(m: ClientMessage): void;
  /** A decoded frame; it is closed when this returns. */
  output(frame: VideoFrame): void;
  /** The host sends a codec this browser cannot decode. */
  unsupported(codec: DeskCodec): void;
}

interface DecoderHandlers {
  output(frame: VideoFrame): void;
  error(e: DOMException): void;
}

/**
 * A decoder configured ahead of the first frame: setting one up costs a few hundred ms the first time
 * (the media stack, the hardware decoder), so the worker starts it while the ticket is still being
 * minted, and the first socket whose stream has that codec adopts it.
 */
let warm: { codec: DeskCodec; decoder: VideoDecoder; adopt(h: DecoderHandlers): void } | null = null;

export function warmDecoder(codec: DeskCodec): void {
  if (warm || typeof VideoDecoder !== "function") return;
  let handlers: DecoderHandlers | null = null;
  try {
    const decoder = new VideoDecoder({
      output: (f) => (handlers ? handlers.output(f) : f.close()),
      error: (e) => handlers?.error(e),
    });
    decoder.configure({ codec: WEBCODECS[codec], optimizeForLatency: true, hardwareAcceleration: "no-preference" });
    warm = {
      codec,
      decoder,
      adopt: (h) => {
        handlers = h;
      },
    };
  } catch {
    // no warm-up then
  }
}

function takeWarm(codec: DeskCodec, handlers: DecoderHandlers): VideoDecoder | null {
  if (!warm || warm.codec !== codec || warm.decoder.state !== "configured") return null;
  const d = warm.decoder;
  warm.adopt(handlers);
  warm = null;
  return d;
}

export class DeskVideo {
  codec: DeskCodec | null = null;
  bytes = 0;
  private decoder: VideoDecoder | null = null;
  private waitingKey = true;
  private closed = false;
  /** Which H.264 codec string the next decoder uses (High, Main, Baseline). */
  private h264Try = 0;
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
      // Nothing to show until a key frame; keep the host going. The gateway sends a joining socket
      // its last key frame and asks the host itself, so no refresh from here (each one rebuilds the
      // host's encoder); a codec switch starts at a key frame too.
      if (f.last) this.host.send({ type: "ack" });
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
    const handlers: DecoderHandlers = {
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
          if (codec === "h264" && this.h264Try < H264_FALLBACKS.length) {
            this.h264Try++;
            this.recover();
            return;
          }
          this.host.unsupported(codec);
          return;
        }
        this.recover();
      },
    };
    // The warm decoder is configured with the first (High) string, so only a first try adopts it.
    const adopted = codec !== "h264" || this.h264Try === 0 ? takeWarm(codec, handlers) : null;
    if (adopted) {
      this.decoder = adopted;
      this.codec = codec;
      return true;
    }
    this.decoder = new VideoDecoder(handlers);
    // Annex B H.264 and VP9/AV1 need no description. H.264 is configured as High, then Main, then
    // Baseline if the decoder refuses (it says so through `error`, see there).
    const string = codec === "h264" ? [WEBCODECS.h264, ...H264_FALLBACKS][this.h264Try] : WEBCODECS[codec];
    try {
      this.decoder.configure({ codec: string!, optimizeForLatency: true, hardwareAcceleration: "no-preference" });
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
    // The stream broke here, not at a join: the gateway will not ask for us.
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
