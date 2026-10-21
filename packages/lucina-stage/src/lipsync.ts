/**
 * Audio-driven lip-sync: RMS envelope + a coarse three-band spectral split -> mouth viseme weights,
 * with attack/release smoothing so the jaw never jitters, and an energy-peak detector for small
 * head nods on stressed syllables. The analysis is pure (testable); `AnalyserLipSync` feeds it
 * from a WebAudio AnalyserNode.
 */
import { clamp, VISEMES, zeroVisemes, type VisemeWeights } from './types';
import { attackRelease, damp } from './spring';

export interface AudioFeatures {
  /** RMS of the time-domain frame, linear (0..1 for full-scale audio). */
  rms: number;
  /** Relative energy (sums to 1, or all 0 in silence) in the low/mid/high bands. */
  low: number;
  mid: number;
  high: number;
}

export interface LipSyncConfig {
  /** Below this level (dBFS) the mouth is closed. */
  floorDb: number;
  /** At or above this level (dBFS) the mouth is fully open. */
  fullDb: number;
  /** Band edges in Hz: low = [lowHz, midHz), mid = [midHz, highHz), high = [highHz, topHz). */
  lowHz: number;
  midHz: number;
  highHz: number;
  topHz: number;
  /** Per-band gains that compensate the natural spectral tilt of speech. */
  bandGain: [number, number, number];
  /** Smoothing time constants in seconds. */
  attack: number;
  release: number;
  /** Maximum total mouth weight. */
  maxOpen: number;
}

export const DEFAULT_LIPSYNC: LipSyncConfig = {
  floorDb: -46,
  fullDb: -16,
  lowHz: 90,
  midHz: 700,
  highHz: 1800,
  topHz: 5500,
  bandGain: [1, 2.2, 5],
  attack: 0.045,
  release: 0.11,
  maxOpen: 0.9,
};

export function rmsOf(samples: ArrayLike<number>): number {
  let s = 0;
  for (let i = 0; i < samples.length; i++) {
    const v = samples[i] as number;
    s += v * v;
  }
  return samples.length ? Math.sqrt(s / samples.length) : 0;
}

/**
 * @param time  time-domain samples (-1..1)
 * @param freqDb  AnalyserNode.getFloatFrequencyData output (dB per bin, bins span 0..sampleRate/2)
 */
export function analyzeFrame(
  time: ArrayLike<number>,
  freqDb: ArrayLike<number>,
  sampleRate: number,
  cfg: LipSyncConfig = DEFAULT_LIPSYNC,
): AudioFeatures {
  const rms = rmsOf(time);
  const binHz = sampleRate / 2 / Math.max(freqDb.length, 1);
  const bands = [0, 0, 0];
  const edges = [cfg.lowHz, cfg.midHz, cfg.highHz, cfg.topHz];
  for (let i = 0; i < freqDb.length; i++) {
    const hz = (i + 0.5) * binHz;
    if (hz < cfg.lowHz || hz >= cfg.topHz) continue;
    const db = freqDb[i] as number;
    if (!Number.isFinite(db)) continue;
    const p = Math.pow(10, db / 10);
    const b = hz < (edges[1] as number) ? 0 : hz < (edges[2] as number) ? 1 : 2;
    bands[b] = (bands[b] as number) + p;
  }
  return { rms, ...normalizeBands(bands[0] as number, bands[1] as number, bands[2] as number, cfg) };
}

export function normalizeBands(
  low: number,
  mid: number,
  high: number,
  cfg: LipSyncConfig = DEFAULT_LIPSYNC,
): { low: number; mid: number; high: number } {
  const l = Math.sqrt(Math.max(low, 0)) * cfg.bandGain[0];
  const m = Math.sqrt(Math.max(mid, 0)) * cfg.bandGain[1];
  const h = Math.sqrt(Math.max(high, 0)) * cfg.bandGain[2];
  const t = l + m + h;
  if (!(t > 0)) return { low: 0, mid: 0, high: 0 };
  return { low: l / t, mid: m / t, high: h / t };
}

/** 0 (closed) .. 1 (fully open) from the RMS level. */
export function openness(rms: number, cfg: LipSyncConfig = DEFAULT_LIPSYNC): number {
  if (!(rms > 0)) return 0;
  const db = 20 * Math.log10(rms);
  const x = clamp((db - cfg.floorDb) / (cfg.fullDb - cfg.floorDb), 0, 1);
  // Ease-in so quiet breath noise barely opens the mouth.
  return x * x * (3 - 2 * x);
}

/**
 * Instantaneous (unsmoothed) viseme weights. Heuristic formant mapping:
 * low-heavy -> o/u (rounded), mid-heavy -> a (open), high-heavy -> i/e (spread).
 */
export function visemesFromFeatures(f: AudioFeatures, cfg: LipSyncConfig = DEFAULT_LIPSYNC): VisemeWeights {
  const open = openness(f.rms, cfg);
  if (open <= 0) return zeroVisemes();
  const { low: l, mid: m, high: h } = f;
  const raw: VisemeWeights = {
    a: m * 1.1 + l * 0.25 + open * 0.35,
    o: l * 0.85,
    u: l * 0.55 * (1 - open),
    e: m * 0.35 + h * 0.6,
    i: h * 0.9,
  };
  // Sharpen so one shape dominates (cleaner mouth), then normalise.
  let sum = 0;
  for (const v of VISEMES) {
    raw[v] = raw[v] * raw[v];
    sum += raw[v];
  }
  const out = zeroVisemes();
  if (sum <= 0) return out;
  const total = open * cfg.maxOpen;
  for (const v of VISEMES) out[v] = (raw[v] / sum) * total;
  return out;
}

/** Attack/release smoothing of the viseme weights. */
export class VisemeSmoother {
  readonly weights: VisemeWeights = zeroVisemes();

  constructor(private readonly cfg: LipSyncConfig = DEFAULT_LIPSYNC) {}

  update(target: VisemeWeights, dt: number): VisemeWeights {
    for (const v of VISEMES) {
      this.weights[v] = attackRelease(this.weights[v], target[v], this.cfg.attack, this.cfg.release, dt);
      if (this.weights[v] < 3e-3 && target[v] === 0) this.weights[v] = 0; // snap shut, invisible below this
    }
    return this.weights;
  }

  reset(): void {
    for (const v of VISEMES) this.weights[v] = 0;
  }
}

/** Detects energy peaks (stressed syllables) for small head nods. */
export class StressDetector {
  private fast = 0;
  private slow = 0;
  private armed = true;
  private sinceLast = Infinity;

  constructor(
    private readonly ratio = 1.35,
    private readonly minOpen = 0.35,
    private readonly refractory = 0.28,
    private readonly cfg: LipSyncConfig = DEFAULT_LIPSYNC,
  ) {}

  /** Returns a peak strength (0..1) when a stressed syllable starts, otherwise 0. */
  update(rms: number, dt: number): number {
    this.fast = damp(this.fast, rms, 0.03, dt);
    this.slow = damp(this.slow, rms, 0.45, dt);
    this.sinceLast += dt;
    const open = openness(this.fast, this.cfg);
    if (!this.armed && this.fast < this.slow * 1.08) this.armed = true;
    if (this.armed && this.sinceLast > this.refractory && open > this.minOpen && this.fast > this.slow * this.ratio) {
      this.armed = false;
      this.sinceLast = 0;
      return clamp((this.fast / Math.max(this.slow, 1e-6) - 1) * 0.8, 0.2, 1);
    }
    return 0;
  }

  reset(): void {
    this.fast = 0;
    this.slow = 0;
    this.armed = true;
    this.sinceLast = Infinity;
  }
}

export interface LipSyncFrame {
  weights: VisemeWeights;
  /** Smoothed openness 0..1. */
  open: number;
  /** >0 on a stressed-syllable onset. */
  stress: number;
}

/** Feature stream -> smoothed visemes + stress events. */
export class LipSyncProcessor {
  readonly smoother: VisemeSmoother;
  readonly stress: StressDetector;
  private open = 0;

  constructor(readonly cfg: LipSyncConfig = DEFAULT_LIPSYNC) {
    this.smoother = new VisemeSmoother(cfg);
    this.stress = new StressDetector(1.35, 0.35, 0.28, cfg);
  }

  process(f: AudioFeatures, dt: number): LipSyncFrame {
    const target = visemesFromFeatures(f, this.cfg);
    const weights = this.smoother.update(target, dt);
    this.open = attackRelease(this.open, openness(f.rms, this.cfg), this.cfg.attack, this.cfg.release, dt);
    return { weights, open: this.open, stress: this.stress.update(f.rms, dt) };
  }

  /** Lets the mouth close smoothly when no audio is playing. */
  silence(dt: number): LipSyncFrame {
    return this.process({ rms: 0, low: 0, mid: 0, high: 0 }, dt);
  }

  reset(): void {
    this.smoother.reset();
    this.stress.reset();
    this.open = 0;
  }
}

/** Reads an AnalyserNode each frame and runs the processor. */
export class AnalyserLipSync {
  readonly processor: LipSyncProcessor;
  private readonly time: Float32Array<ArrayBuffer>;
  private readonly freq: Float32Array<ArrayBuffer>;

  constructor(
    readonly analyser: AnalyserNode,
    cfg: LipSyncConfig = DEFAULT_LIPSYNC,
  ) {
    analyser.fftSize = 1024;
    analyser.smoothingTimeConstant = 0.2;
    this.time = new Float32Array(analyser.fftSize);
    this.freq = new Float32Array(analyser.frequencyBinCount);
    this.processor = new LipSyncProcessor(cfg);
  }

  sample(dt: number): LipSyncFrame {
    this.analyser.getFloatTimeDomainData(this.time);
    this.analyser.getFloatFrequencyData(this.freq);
    const f = analyzeFrame(this.time, this.freq, this.analyser.context.sampleRate, this.processor.cfg);
    return this.processor.process(f, dt);
  }
}
