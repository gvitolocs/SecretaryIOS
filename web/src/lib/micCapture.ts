// The microphone at 16 kHz s16le, in chunks, from an AudioWorklet. Shared by Lucina Prompter
// (prompterLive.ts, 100 ms chunks) and the streaming voice socket (voiceSocket.ts, 60 ms).
// The worklet is inline, so no extra file is served.

export const SAMPLE_RATE = 16_000;

/** Samples (and so 16 kHz frames) in a chunk of `ms` milliseconds. */
export function chunkSamples(ms: number): number {
  return Math.max(1, Math.round((SAMPLE_RATE * ms) / 1000));
}

/** Bytes of s16le in a chunk of `ms` milliseconds. */
export function chunkBytes(ms: number): number {
  return chunkSamples(ms) * 2;
}

/**
 * Worklet source: downsamples the device rate (44.1/48 kHz) to 16 kHz by averaging, and posts
 * chunks of `chunk` samples of signed 16-bit PCM plus their level.
 */
export function workletSource(name: string, chunk: number): string {
  return `
class MicTap extends AudioWorkletProcessor {
  constructor(o) { super(); this.ratio = sampleRate / ${SAMPLE_RATE}; this.acc = 0; this.n = 0; this.pos = 0;
    this.out = new Int16Array(${chunk}); this.k = 0; this.sq = 0; }
  process(inputs) {
    const ch = inputs[0] && inputs[0][0];
    if (!ch) return true;
    for (let i = 0; i < ch.length; i++) {
      this.acc += ch[i]; this.n++; this.pos++;
      if (this.pos >= this.ratio) {
        this.pos -= this.ratio;
        const v = Math.max(-1, Math.min(1, this.acc / this.n));
        this.acc = 0; this.n = 0; this.sq += v * v;
        this.out[this.k++] = v < 0 ? v * 0x8000 : v * 0x7fff;
        if (this.k === ${chunk}) {
          this.port.postMessage({ pcm: this.out.buffer, level: Math.sqrt(this.sq / ${chunk}) }, [this.out.buffer]);
          this.out = new Int16Array(${chunk}); this.k = 0; this.sq = 0;
        }
      }
    }
    return true;
  }
}
registerProcessor(${JSON.stringify(name)}, MicTap);
`;
}

export interface Microphone {
  stop(): void;
}

export interface MicrophoneOptions {
  /** Chunk length in ms (default 100). */
  chunkMs?: number;
}

/** Open the microphone; `onChunk` gets a chunk of 16 kHz PCM and its level (0..1). */
export async function openMicrophone(
  onChunk: (pcm: ArrayBuffer, level: number) => void,
  opts: MicrophoneOptions = {},
): Promise<Microphone> {
  const chunkMs = opts.chunkMs ?? 100;
  const name = `mic-tap-${chunkMs}`;
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: {
      channelCount: 1,
      echoCancellation: true,
      noiseSuppression: true,
      autoGainControl: true,
    },
  });
  const ctx = new AudioContext();
  const url = URL.createObjectURL(new Blob([workletSource(name, chunkSamples(chunkMs))], { type: "text/javascript" }));
  let src: MediaStreamAudioSourceNode;
  let tap: AudioWorkletNode;
  try {
    await ctx.audioWorklet.addModule(url);
    src = ctx.createMediaStreamSource(stream);
    tap = new AudioWorkletNode(ctx, name, { numberOfInputs: 1, numberOfOutputs: 0 });
  } catch (e) {
    stream.getTracks().forEach((t) => t.stop());
    void ctx.close();
    throw e;
  } finally {
    URL.revokeObjectURL(url);
  }
  tap.port.onmessage = (e: MessageEvent<{ pcm: ArrayBuffer; level: number }>) => onChunk(e.data.pcm, e.data.level);
  src.connect(tap);
  if (ctx.state === "suspended") await ctx.resume();
  return {
    stop() {
      tap.port.onmessage = null;
      src.disconnect();
      stream.getTracks().forEach((t) => t.stop());
      void ctx.close();
    },
  };
}
