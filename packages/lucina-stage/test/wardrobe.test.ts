// @vitest-environment jsdom
import { Group, PerspectiveCamera } from 'three';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { effectPhases, EFFECT_PEAK, playOutfitEffect } from '../src/outfitEffect';
import type { LucinaStage, LucinaStageOptions } from '../src/stage';
import type { StageState } from '../src/types';
import { createWardrobeStage, outfitName, outfitUrls, parseWardrobe, type StageFactory, type WardrobeEntry } from '../src/wardrobe';

const WARDROBE: WardrobeEntry[] = [
  { id: 'uniform', name: { it: 'Divisa', en: 'Uniform' }, glb: 'lucina.glb', thumbnail: 'outfit-uniform.webp', tags: ['uniform'] },
  { id: 'gym', name: { en: 'Gym' }, glb: 'lucina-gym.glb', tags: ['sport'] },
  { id: 'maid', name: 'Maid', glb: 'lucina-maid.glb', manifest: 'manifest-maid.json' },
];

interface Fake extends LucinaStage {
  opts: LucinaStageOptions;
  canvas: HTMLCanvasElement;
  disposed: boolean;
  emit(next: StageState): void;
  tap(): void;
  calls: string[];
}

function fakeFactory(log: Fake[], fail?: (url: string) => boolean): StageFactory {
  return async (canvas, opts) => {
    await Promise.resolve();
    if (fail?.(opts.modelUrl ?? '')) throw new Error(`load failed: ${opts.modelUrl}`);
    let state: StageState = 'idle';
    let muted = !!opts.muted;
    const stateCbs = new Set<(n: StageState, p: StageState) => void>();
    const tapCbs = new Set<(e: never) => void>();
    const calls: string[] = [];
    const f = {
      opts,
      canvas,
      disposed: false,
      calls,
      get state() {
        return state;
      },
      setState(s: StageState) {
        const p = state;
        state = s;
        stateCbs.forEach((cb) => cb(s, p));
      },
      emit(s: StageState) {
        f.setState(s);
      },
      tap() {
        tapCbs.forEach((cb) => cb({ object: 'x', point: { x: 0, y: 0, z: 0 }, clientX: 0, clientY: 0 } as never));
      },
      onStateChange(cb: (n: StageState, p: StageState) => void) {
        stateCbs.add(cb);
        return () => stateCbs.delete(cb);
      },
      onTap(cb: (e: never) => void) {
        tapCbs.add(cb);
        return () => tapCbs.delete(cb);
      },
      speaking: false,
      setMuted(m: boolean) {
        muted = m;
      },
      isMuted: () => muted,
      setExpression: (e: unknown) => calls.push(`expr:${String(e)}`),
      lookAtPointer: (on: boolean) => calls.push(`look:${on}`),
      setFraming: (fr: string) => calls.push(`framing:${fr}`),
      setTheme: (t: string) => calls.push(`theme:${t}`),
      setQuality: (q: string) => calls.push(`quality:${q}`),
      setReducedMotion: () => {},
      gesture: (n: string) => (calls.push(`gesture:${n}`), true),
      dispose() {
        f.disposed = true;
      },
      info: { placeholder: false, clips: [], extensions: [], rig: {} as never, hidden: [] },
      three: { model: new Group(), camera: new PerspectiveCamera() },
    } as unknown as Fake;
    log.push(f);
    return f;
  };
}

let host: HTMLDivElement;
let canvas: HTMLCanvasElement;
beforeEach(() => {
  host = document.createElement('div');
  canvas = document.createElement('canvas');
  canvas.className = 'stage-canvas';
  host.appendChild(canvas);
  document.body.appendChild(host);
});
afterEach(() => {
  host.remove();
});

describe('parseWardrobe', () => {
  it('keeps valid entries only', () => {
    const list = parseWardrobe([
      ...WARDROBE,
      { id: 'gym', glb: 'dup.glb' },
      { id: 'Bad', glb: 'x.glb' },
      { id: 'evil', glb: '../x.glb' },
      { id: 'pic', glb: 'x.png' },
      { id: 'thumb', glb: 'x.glb', thumbnail: 'x.json', tags: ['a', 3, ''] },
      null,
      'x',
    ]);
    expect(list.map((e) => e.id)).toEqual(['uniform', 'gym', 'maid', 'thumb']);
    expect(list[3]).toEqual({ id: 'thumb', glb: 'x.glb', tags: ['a'] });
    expect(parseWardrobe({})).toEqual([]);
  });
  it('names and urls', () => {
    const [u, g, m] = parseWardrobe(WARDROBE);
    expect(outfitName(u!, 'it')).toBe('Divisa');
    expect(outfitName(u!, 'da')).toBe('Uniform');
    expect(outfitName(m!, 'it')).toBe('Maid');
    expect(outfitUrls(g!, '/v1/assets', '/v1/assets/manifest.json')).toEqual({
      modelUrl: '/v1/assets/lucina-gym.glb',
      manifestUrl: '/v1/assets/manifest.json',
    });
    expect(outfitUrls(m!, 'lucina-app://stage/v1/assets/').manifestUrl).toBe('lucina-app://stage/v1/assets/manifest-maid.json');
  });
});

describe('createWardrobeStage', () => {
  const base = { manifestUrl: '/v1/assets/manifest.json', wardrobe: WARDROBE, changeEffect: false, clipsUrl: '/v1/assets/lucina-anims.glb' };

  it('starts in the requested outfit and swaps on a second canvas', async () => {
    const log: Fake[] = [];
    const s = await createWardrobeStage(canvas, { ...base, outfit: 'gym', framing: 'full', muted: true }, fakeFactory(log));
    expect(log[0]!.opts.modelUrl).toBe('/v1/assets/lucina-gym.glb');
    expect(log[0]!.opts).not.toHaveProperty('wardrobe');
    expect(s.outfit).toBe('gym');

    const states: StageState[] = [];
    let taps = 0;
    s.onStateChange((n) => states.push(n));
    s.onTap(() => taps++);
    const outfits: (string | null)[] = [];
    s.onOutfitChange((o) => outfits.push(o));
    const changing: boolean[] = [];
    s.onChanging((c) => changing.push(c));
    s.setMuted(false);
    s.setTheme('light');
    s.setQuality('low');
    s.setFraming('bust');
    s.lookAtPointer(true);
    s.setState('thinking');

    await s.setOutfit('maid');
    expect(log).toHaveLength(2);
    const [old, next] = log;
    expect(next!.opts).toMatchObject({
      modelUrl: '/v1/assets/lucina-maid.glb',
      manifestUrl: '/v1/assets/manifest-maid.json',
      clipsUrl: '/v1/assets/lucina-anims.glb',
      framing: 'bust',
      muted: false,
      fallbackToPlaceholder: false,
    });
    expect(next!.canvas).not.toBe(canvas);
    expect(next!.canvas.parentElement).toBe(host);
    expect(next!.canvas.style.opacity).toBe('1');
    expect(next!.calls).toContain('look:true');
    expect(next!.state).toBe('thinking');
    expect(old!.disposed).toBe(true);
    expect(canvas.style.visibility).toBe('hidden');
    expect(s.outfit).toBe('maid');
    expect(outfits).toEqual(['maid']);
    expect(changing).toEqual([true, false]);

    // Listeners follow the new stage; the old one no longer reaches them.
    states.length = 0;
    next!.emit('listening');
    old!.emit('speaking');
    next!.tap();
    old!.tap();
    expect(states).toEqual(['listening']);
    expect(taps).toBe(1);

    // A third swap removes the canvas the wrapper made.
    await s.setOutfit('uniform');
    expect(next!.disposed).toBe(true);
    expect(next!.canvas.isConnected).toBe(false);
    expect(log[2]!.opts.manifestUrl).toBe('/v1/assets/manifest.json');
    s.dispose();
    expect(log[2]!.disposed).toBe(true);
    expect(host.querySelectorAll('canvas')).toHaveLength(1);
    expect(canvas.style.visibility).toBe('');
  });

  it('is a no-op for the outfit on stage, rejects unknown ids, keeps the old outfit on failure', async () => {
    const log: Fake[] = [];
    const s = await createWardrobeStage(canvas, { ...base, outfit: 'uniform' }, fakeFactory(log, (u) => u.includes('gym')));
    await s.setOutfit('uniform');
    expect(log).toHaveLength(1);
    await expect(s.setOutfit('pirate')).rejects.toThrow(/Unknown outfit/);
    await expect(s.setOutfit('gym')).rejects.toThrow(/load failed/);
    expect(s.outfit).toBe('uniform');
    expect(log[0]!.disposed).toBe(false);
    expect(host.querySelectorAll('canvas')).toHaveLength(1);
    expect(s.changing).toBe(false);
    await s.setOutfit('maid');
    expect(s.outfit).toBe('maid');
  });

  it('latest wins', async () => {
    const log: Fake[] = [];
    const s = await createWardrobeStage(canvas, { ...base, outfit: 'uniform' }, fakeFactory(log));
    const a = s.setOutfit('gym');
    const b = s.setOutfit('maid');
    await Promise.all([a, b]);
    expect(s.outfit).toBe('maid');
    // The gym swap was superseded before it loaded anything.
    expect(log.map((f) => f.opts.modelUrl)).toEqual(['/v1/assets/lucina.glb', '/v1/assets/lucina-maid.glb']);
    expect(host.querySelectorAll('canvas:not([style*="hidden"])')).toHaveLength(1);
  });

  it('waits for the current sentence before swapping', async () => {
    vi.useFakeTimers();
    try {
      const log: Fake[] = [];
      const s = await createWardrobeStage(canvas, { ...base, outfit: 'uniform' }, fakeFactory(log));
      (log[0] as unknown as { speaking: boolean }).speaking = true;
      let done = false;
      void s.setOutfit('gym').then(() => (done = true));
      await vi.advanceTimersByTimeAsync(1000);
      expect(done).toBe(false);
      expect(log[0]!.disposed).toBe(false);
      (log[0] as unknown as { speaking: boolean }).speaking = false;
      await vi.advanceTimersByTimeAsync(1000);
      expect(done).toBe(true);
      expect(s.outfit).toBe('gym');
    } finally {
      vi.useRealTimers();
    }
  });

  it('accepts a manifest object (blob URL) and plain model URLs', async () => {
    const log: Fake[] = [];
    const created: string[] = [];
    const revoked: string[] = [];
    URL.createObjectURL = vi.fn(() => (created.push('blob:x'), 'blob:x'));
    URL.revokeObjectURL = vi.fn((u: string) => void revoked.push(u));
    const s = await createWardrobeStage(canvas, { ...base, modelUrl: '/v1/assets/lucina.glb' }, fakeFactory(log));
    expect(s.outfit).toBeNull();
    await s.setModel('/v1/assets/other.glb', { morphs: {} });
    expect(log[1]!.opts.manifestUrl).toBe('blob:x');
    expect(revoked).toEqual(['blob:x']);
    expect(s.modelUrl).toBe('/v1/assets/other.glb');
    expect(s.outfit).toBeNull();
  });
  it('tells each stage its outfit, hairstyle and quality for the physics variants', async () => {
    const log: Fake[] = [];
    const s = await createWardrobeStage(canvas, { ...base, outfit: 'uniform' }, fakeFactory(log));
    expect(log[0]!.opts.physics).toEqual({ outfit: 'uniform', hair: 'natural' });
    s.setQuality('low');
    await s.setOutfit('gym');
    expect(log[1]!.opts.physics).toEqual({ outfit: 'gym', hair: 'natural', quality: 'low' });
    const off: Fake[] = [];
    await createWardrobeStage(canvas, { ...base, outfit: 'gym', physics: false }, fakeFactory(off));
    expect(off[0]!.opts.physics).toBe(false);
  });
});

describe('outfit effect', () => {
  it('peaks once and ends', () => {
    const at = (p: number) => effectPhases(p);
    expect(at(0).column).toBe(0);
    expect(at(EFFECT_PEAK).column).toBeCloseTo(1);
    expect(at(EFFECT_PEAK).flash).toBeCloseTo(1);
    expect(at(0).flash).toBeLessThan(0.01);
    expect(at(1).fade).toBe(0);
    expect(at(EFFECT_PEAK + 0.3).burst).toBeGreaterThan(at(EFFECT_PEAK + 0.1).burst);
  });
  it('reduced motion swaps at once without drawing', async () => {
    let peaks = 0;
    const fx = playOutfitEffect({ host: canvas, reducedMotion: true, onPeak: () => peaks++ });
    await fx.done;
    expect(peaks).toBe(1);
    expect(host.querySelectorAll('canvas')).toHaveLength(1);
  });
});
