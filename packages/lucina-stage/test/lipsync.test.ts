import { describe, expect, it } from 'vitest';
import {
  analyzeFrame,
  LipSyncProcessor,
  openness,
  StressDetector,
  visemesFromFeatures,
  VisemeSmoother,
  type AudioFeatures,
} from '../src/lipsync';
import { VISEMES, zeroVisemes, type VisemeWeights } from '../src/types';

const sum = (w: VisemeWeights): number => VISEMES.reduce((s, v) => s + w[v], 0);
const argmax = (w: VisemeWeights): string => VISEMES.reduce((a, b) => (w[b] > w[a] ? b : a));
const loud = (low: number, mid: number, high: number, rms = 0.2): AudioFeatures => {
  const t = low + mid + high;
  return { rms, low: low / t, mid: mid / t, high: high / t };
};

describe('envelope -> viseme weights', () => {
  it('silence closes the mouth', () => {
    expect(sum(visemesFromFeatures({ rms: 0, low: 0, mid: 0, high: 0 }))).toBe(0);
    expect(sum(visemesFromFeatures(loud(1, 1, 1, 0.001)))).toBe(0); // below the floor
    expect(openness(0)).toBe(0);
  });

  it('openness grows monotonically with level and saturates', () => {
    const levels = [0.005, 0.01, 0.03, 0.08, 0.2, 0.5];
    const o = levels.map((l) => openness(l));
    for (let i = 1; i < o.length; i++) expect(o[i]!).toBeGreaterThanOrEqual(o[i - 1]!);
    expect(o[o.length - 1]).toBe(1);
  });

  it('maps band balance to plausible shapes', () => {
    expect(argmax(visemesFromFeatures(loud(0.1, 1, 0.1)))).toBe('a');
    expect(argmax(visemesFromFeatures(loud(0.1, 0.1, 1)))).toBe('i');
    expect(['o', 'u']).toContain(argmax(visemesFromFeatures(loud(1, 0.05, 0.02))));
  });

  it('total weight never exceeds maxOpen', () => {
    for (const f of [loud(1, 1, 1, 1), loud(1, 0, 0, 1), loud(0, 0, 1, 1)]) {
      expect(sum(visemesFromFeatures(f))).toBeLessThanOrEqual(0.9 + 1e-9);
    }
  });

  it('analyzeFrame splits spectral energy into bands', () => {
    const sr = 48000;
    const bins = 512; // fftSize 1024
    const binHz = sr / 2 / bins;
    const freq = new Float32Array(bins).fill(-120);
    freq[Math.round(2500 / binHz)] = -20; // a strong high tone
    const time = new Float32Array(1024).map((_, i) => 0.3 * Math.sin(i));
    const f = analyzeFrame(time, freq, sr);
    expect(f.high).toBeGreaterThan(0.9);
    expect(f.rms).toBeGreaterThan(0.2);
  });
});

describe('smoothing', () => {
  it('attack is smoothed (no instant jumps) and release closes the mouth', () => {
    const s = new VisemeSmoother();
    const target = { ...zeroVisemes(), a: 0.8 };
    const first = s.update(target, 1 / 60).a;
    expect(first).toBeGreaterThan(0);
    expect(first).toBeLessThan(0.4);
    for (let i = 0; i < 30; i++) s.update(target, 1 / 60);
    expect(s.weights.a).toBeGreaterThan(0.75);
    for (let i = 0; i < 30; i++) s.update(zeroVisemes(), 1 / 60); // 0.5 s of silence
    expect(sum(s.weights)).toBeLessThan(0.02);
  });

  it('release is slower than attack', () => {
    const up = new VisemeSmoother();
    up.update({ ...zeroVisemes(), a: 1 }, 0.05);
    const down = new VisemeSmoother();
    down.weights.a = 1;
    down.update(zeroVisemes(), 0.05);
    expect(up.weights.a).toBeGreaterThan(1 - down.weights.a);
  });

  it('removes frame-to-frame jitter', () => {
    const p = new LipSyncProcessor();
    let rawJitter = 0;
    let smoothJitter = 0;
    let prevRaw = 0;
    let prevSmooth = 0;
    for (let i = 0; i < 240; i++) {
      const f = loud(0.3, 1, 0.3, i % 2 ? 0.25 : 0.02); // alternating loud/quiet every frame
      const raw = sum(visemesFromFeatures(f));
      const smooth = sum(p.process(f, 1 / 60).weights);
      if (i > 0) {
        rawJitter += Math.abs(raw - prevRaw);
        smoothJitter += Math.abs(smooth - prevSmooth);
      }
      prevRaw = raw;
      prevSmooth = smooth;
    }
    expect(smoothJitter).toBeLessThan(rawJitter * 0.5);
  });

  it('processor.silence() closes the mouth after speech', () => {
    const p = new LipSyncProcessor();
    for (let i = 0; i < 30; i++) p.process(loud(0.2, 1, 0.2, 0.3), 1 / 60);
    let w = p.process(loud(0.2, 1, 0.2, 0.3), 1 / 60).weights;
    expect(sum(w)).toBeGreaterThan(0.5);
    for (let i = 0; i < 40; i++) w = p.silence(1 / 60).weights;
    expect(sum(w)).toBe(0);
  });
});

describe('stress detector', () => {
  it('fires on energy onsets with a refractory period, not on steady sound', () => {
    const d = new StressDetector();
    let hits = 0;
    for (let i = 0; i < 120; i++) if (d.update(0.15, 1 / 60) > 0) hits++; // steady after the first onset
    expect(hits).toBeLessThanOrEqual(1);
    const d2 = new StressDetector();
    hits = 0;
    for (let i = 0; i < 240; i++) {
      const syllable = Math.floor(i / 15) % 2 === 0; // 4 Hz syllables
      if (d2.update(syllable ? 0.3 : 0.004, 1 / 60) > 0) hits++;
    }
    expect(hits).toBeGreaterThanOrEqual(4);
    expect(hits).toBeLessThanOrEqual(9);
  });
});
