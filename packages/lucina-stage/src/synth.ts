/**
 * Speech-like test audio (no TTS needed): a glottal pulse train through two formant resonators,
 * shaped into syllables with varying vowels and stress. Returns a 16-bit PCM WAV ArrayBuffer that
 * `stage.speak()` can play. Pure TS: runs in the browser and in Node (dev mock TTS).
 */

const VOWELS: Record<string, [number, number]> = {
  a: [800, 1250],
  i: [300, 2300],
  u: [330, 800],
  e: [480, 1900],
  o: [500, 880],
};

export interface SynthOptions {
  sampleRate?: number;
  /** Vowel sequence, one per syllable. */
  pattern?: string;
  /** Pitch in Hz. */
  f0?: number;
  seed?: number;
}

function resonator(freq: number, bw: number, sr: number): (x: number) => number {
  const r = Math.exp((-Math.PI * bw) / sr);
  const a1 = 2 * r * Math.cos((2 * Math.PI * freq) / sr);
  const a2 = -r * r;
  const g = 1 - r;
  let y1 = 0;
  let y2 = 0;
  return (x) => {
    const y = g * x + a1 * y1 + a2 * y2;
    y2 = y1;
    y1 = y;
    return y;
  };
}

export function synthSpeechSamples(opts: SynthOptions = {}): { samples: Float32Array; sampleRate: number } {
  const sr = opts.sampleRate ?? 22050;
  const pattern = opts.pattern ?? 'ciao a te. o-i-e-a-u, ecco!';
  const f0 = opts.f0 ?? 215;
  let seed = opts.seed ?? 7;
  const rnd = (): number => {
    seed = (seed * 1664525 + 1013904223) >>> 0;
    return seed / 4294967296;
  };
  // Syllable plan: [vowel | null for pause, duration s, stress 0..1]
  const plan: [string | null, number, number][] = [];
  for (const ch of pattern.toLowerCase()) {
    if (ch in VOWELS) plan.push([ch, 0.13 + rnd() * 0.09, rnd() < 0.3 ? 1 : 0.55]);
    else if (ch === ' ' || ch === '-') plan.push([null, 0.06, 0]);
    else if (ch === '.' || ch === ',' || ch === '!' || ch === '?') plan.push([null, 0.28, 0]);
  }
  const total = plan.reduce((s, p) => s + p[1], 0.25);
  const out = new Float32Array(Math.ceil(total * sr));
  let pos = Math.floor(0.05 * sr);
  let phase = 0;
  for (const [v, dur, stress] of plan) {
    const n = Math.floor(dur * sr);
    if (!v) {
      pos += n;
      continue;
    }
    const [f1, f2] = VOWELS[v]!;
    const r1 = resonator(f1, 90, sr);
    const r2 = resonator(f2, 120, sr);
    const r3 = resonator(2900, 200, sr);
    for (let i = 0; i < n && pos + i < out.length; i++) {
      const t = i / n;
      const pitch = f0 * (1 + 0.06 * stress * Math.sin(Math.PI * t) - 0.04 * t);
      phase += pitch / sr;
      let src = 0;
      if (phase >= 1) {
        phase -= 1;
        src = 1;
      }
      src += (rnd() - 0.5) * 0.04;
      const env = Math.sin(Math.PI * Math.min(1, t * 1.15)) ** 0.6 * (0.35 + 0.65 * stress);
      const y = (r1(src) * 1.0 + r2(src) * 0.55 + r3(src) * 0.18) * env;
      out[pos + i] = y;
    }
    pos += n;
  }
  let peak = 0;
  for (const s of out) peak = Math.max(peak, Math.abs(s));
  if (peak > 0) for (let i = 0; i < out.length; i++) out[i] = ((out[i] as number) / peak) * 0.6;
  return { samples: out, sampleRate: sr };
}

export function encodeWav(samples: Float32Array, sampleRate: number): ArrayBuffer {
  const buf = new ArrayBuffer(44 + samples.length * 2);
  const dv = new DataView(buf);
  const str = (o: number, s: string): void => {
    for (let i = 0; i < s.length; i++) dv.setUint8(o + i, s.charCodeAt(i));
  };
  str(0, 'RIFF');
  dv.setUint32(4, 36 + samples.length * 2, true);
  str(8, 'WAVE');
  str(12, 'fmt ');
  dv.setUint32(16, 16, true);
  dv.setUint16(20, 1, true);
  dv.setUint16(22, 1, true);
  dv.setUint32(24, sampleRate, true);
  dv.setUint32(28, sampleRate * 2, true);
  dv.setUint16(32, 2, true);
  dv.setUint16(34, 16, true);
  str(36, 'data');
  dv.setUint32(40, samples.length * 2, true);
  for (let i = 0; i < samples.length; i++) {
    const s = Math.max(-1, Math.min(1, samples[i] as number));
    dv.setInt16(44 + i * 2, s < 0 ? s * 0x8000 : s * 0x7fff, true);
  }
  return buf;
}

export function synthSpeechWav(opts: SynthOptions = {}): ArrayBuffer {
  const { samples, sampleRate } = synthSpeechSamples(opts);
  return encodeWav(samples, sampleRate);
}
