// A short clip from the phone's microphone as the 16 kHz mono PCM WAV that secretary-server's speech-to-text takes.
// MediaRecorder gives AAC/MP4 (Safari) or Opus/WebM (elsewhere), which the server does not decode, so the browser
// does: decodeAudioData, mix to mono, resample to 16 kHz with an OfflineAudioContext, write 16-bit samples.

export const WAV_RATE = 16_000;

/** Mono float samples (-1..1) as a 16-bit PCM WAV file with the canonical 44-byte header. */
export function encodeWav16k(samples: Float32Array, rate = WAV_RATE): Blob {
  const data = new ArrayBuffer(44 + samples.length * 2);
  const v = new DataView(data);
  const ascii = (at: number, s: string) => [...s].forEach((c, i) => v.setUint8(at + i, c.charCodeAt(0)));
  ascii(0, "RIFF");
  v.setUint32(4, 36 + samples.length * 2, true);
  ascii(8, "WAVE");
  ascii(12, "fmt ");
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true); // PCM
  v.setUint16(22, 1, true); // mono
  v.setUint32(24, rate, true);
  v.setUint32(28, rate * 2, true);
  v.setUint16(32, 2, true);
  v.setUint16(34, 16, true);
  ascii(36, "data");
  v.setUint32(40, samples.length * 2, true);
  samples.forEach((s, i) => {
    const x = Math.max(-1, Math.min(1, Number.isFinite(s) ? s : 0));
    v.setInt16(44 + i * 2, Math.round(x < 0 ? x * 0x8000 : x * 0x7fff), true);
  });
  return new Blob([data], { type: "audio/wav" });
}

/** Average the channels of a decoded buffer into one. */
export function mixToMono(channels: Float32Array[]): Float32Array {
  const first = channels[0];
  if (!first) return new Float32Array(0);
  if (channels.length === 1) return first;
  const out = new Float32Array(first.length);
  for (const c of channels) for (let i = 0; i < out.length; i++) out[i]! += (c[i] ?? 0) / channels.length;
  return out;
}

/** Decode `clip` (any format the browser plays) and return it as a 16 kHz mono PCM WAV. */
export async function toWav16k(clip: Blob): Promise<Blob> {
  const Ctx = window.AudioContext ?? (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
  const Offline = window.OfflineAudioContext ?? (window as unknown as { webkitOfflineAudioContext?: typeof OfflineAudioContext }).webkitOfflineAudioContext;
  if (!Ctx || !Offline) throw new Error("This browser can't decode audio.");
  const ctx = new Ctx();
  try {
    const decoded = await ctx.decodeAudioData(await clip.arrayBuffer());
    const length = Math.max(1, Math.ceil(decoded.duration * WAV_RATE));
    const off = new Offline(1, length, WAV_RATE);
    const src = off.createBufferSource();
    src.buffer = decoded;
    src.connect(off.destination);
    src.start();
    const rendered = await off.startRendering();
    return encodeWav16k(mixToMono(Array.from({ length: rendered.numberOfChannels }, (_, i) => rendered.getChannelData(i))));
  } finally {
    void ctx.close();
  }
}
