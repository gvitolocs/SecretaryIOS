/**
 * Lucina's wardrobe on the stage: `createWardrobeStage` is `createLucinaStage` plus
 * `setModel(url, manifest)` / `setOutfit(id)`, which swap the loaded GLB at runtime.
 *
 * How a swap works (the base stage stays untouched, so its toon/material path, clips and scene
 * set are whatever its options say, for every outfit):
 * 1. A second stage is created on a new canvas laid exactly over the current one (invisible),
 *    with the same options (clips, environment, quality, theme, framing, mute) and the new model.
 *    The current outfit keeps animating meanwhile.
 * 2. If she is talking, the swap waits for the sentence to end (at most `maxSpeakWait`).
 * 3. The change effect plays (light column, scanning ring, sparkles; see outfitEffect.ts). At its
 *    flash the new canvas fades in, the old one out; listeners, state and settings carry over.
 * 4. The old stage is disposed and its GPU context released.
 *
 * Latest wins: a newer `setModel` supersedes one that has not started loading. A failed load
 * rejects and leaves the current outfit on stage. Reduced motion: a short crossfade, no effect.
 *
 * Hair is independent of the outfit (hair.ts): `setHair(id)` attaches a hairstyle part to the
 * head bone in place and `setHairColor(id)` tints it, both under the same change effect; an outfit
 * swap carries the current hair and colour over to the new model before it is revealed.
 */
import { Box3, Vector3, type Object3D } from 'three';
import { HairRig, loadHairPart, NATURAL_HAIR, parseHairColors, parseHairstyles, type HairColor, type HairEntry } from './hair';
import { applyToonHairPart, toonFallbackFor } from './toon';
import { playOutfitEffect, type SubjectRect } from './outfitEffect';
import type { StageQualitySetting, StageTheme } from './scene/quality';
import { createLucinaStage, type LucinaStage, type LucinaStageOptions, type TapEvent } from './stage';
import type { Expression, Framing, StageState } from './types';

/** One outfit, as listed by `GET /v1/avatar/wardrobe`. */
export interface WardrobeEntry {
  id: string;
  /** Per language (`it`, `en`, `da`) or one string. */
  name?: Record<string, string> | string;
  /** Asset name of the model, e.g. `lucina-gym.glb`. */
  glb: string;
  /** Asset name of the tile picture (`.png` / `.webp`). */
  thumbnail?: string;
  tags?: string[];
  /** Per-outfit manifest; otherwise reuse the starting toon look (or the stage's rig manifest). */
  manifest?: string;
}

const ID_RE = /^[a-z0-9][a-z0-9_-]{0,39}$/;
const ASSET_RE = /^[a-z0-9][a-z0-9._-]{0,127}$/;

function assetName(v: unknown, exts: string[]): v is string {
  return typeof v === 'string' && ASSET_RE.test(v) && !v.includes('..') && exts.some((e) => v.endsWith(e));
}

/** The whole wardrobe file: `{outfits, hairstyles, colors}` (a bare array is the outfits). */
export function parseWardrobeFile(raw: unknown): { outfits: WardrobeEntry[]; hairstyles: HairEntry[]; colors: HairColor[] } {
  const o = (raw && typeof raw === 'object' && !Array.isArray(raw) ? raw : {}) as Record<string, unknown>;
  return {
    outfits: parseWardrobe(Array.isArray(raw) ? raw : o.outfits),
    hairstyles: parseHairstyles(o.hairstyles),
    colors: parseHairColors(o.colors),
  };
}

/** Valid outfits of an untrusted list (unique ids, order kept); also takes the whole file. */
export function parseWardrobe(raw: unknown): WardrobeEntry[] {
  if (raw && typeof raw === 'object' && !Array.isArray(raw)) return parseWardrobe((raw as { outfits?: unknown }).outfits);
  if (!Array.isArray(raw)) return [];
  const out: WardrobeEntry[] = [];
  const seen = new Set<string>();
  for (const r of raw) {
    if (!r || typeof r !== 'object') continue;
    const o = r as Record<string, unknown>;
    if (typeof o.id !== 'string' || !ID_RE.test(o.id) || seen.has(o.id) || !assetName(o.glb, ['.glb'])) continue;
    const e: WardrobeEntry = { id: o.id, glb: o.glb };
    if (typeof o.name === 'string') e.name = o.name.slice(0, 80);
    else if (o.name && typeof o.name === 'object') {
      const names: Record<string, string> = {};
      for (const [k, v] of Object.entries(o.name as Record<string, unknown>)) {
        if (/^[a-z]{2}$/.test(k) && typeof v === 'string' && v.trim()) names[k] = v.trim().slice(0, 80);
      }
      if (Object.keys(names).length) e.name = names;
    }
    if (assetName(o.thumbnail, ['.png', '.webp'])) e.thumbnail = o.thumbnail;
    if (assetName(o.manifest, ['.json'])) e.manifest = o.manifest;
    if (Array.isArray(o.tags)) {
      const tags = o.tags.filter((t): t is string => typeof t === 'string' && t.length > 0 && t.length <= 32).slice(0, 16);
      if (tags.length) e.tags = tags;
    }
    seen.add(e.id);
    out.push(e);
  }
  return out;
}

/** Display name in `lang` (then English, then any, then the id). */
export function outfitName(e: WardrobeEntry, lang: string): string {
  if (typeof e.name === 'string') return e.name;
  const n = e.name ?? {};
  return n[lang] ?? n.en ?? Object.values(n)[0] ?? e.id;
}

/** URLs of an entry under `assetBase` (e.g. `/v1/assets/`). */
export function outfitUrls(e: WardrobeEntry, assetBase: string, fallbackManifest?: string): { modelUrl: string; manifestUrl?: string } {
  const base = assetBase.endsWith('/') ? assetBase : `${assetBase}/`;
  const manifestUrl = e.manifest ? base + e.manifest : fallbackManifest;
  return manifestUrl ? { modelUrl: base + e.glb, manifestUrl } : { modelUrl: base + e.glb };
}

export interface WardrobeStageOptions extends LucinaStageOptions {
  wardrobe?: WardrobeEntry[];
  /** Where `glb`/`manifest`/`thumbnail` names live (default `/v1/assets/`). */
  assetBase?: string;
  /** Outfit to start in (overrides `modelUrl` when the wardrobe has it). */
  outfit?: string | null;
  /** Play the change effect on swaps (default true; never in reduced motion). */
  changeEffect?: boolean;
  /** Longest wait (ms) for her current sentence before a swap (default 20000). */
  maxSpeakWait?: number;
  /** Hairstyles (`hairstyles` of the wardrobe file). */
  hairstyles?: HairEntry[];
  hairColors?: HairColor[];
  /** Hairstyle to start with (`natural` or null: the model's own hair). */
  hair?: string | null;
  /** Hair colour id to start with (null: natural). */
  hairColor?: string | null;
  /** Loads a hairstyle part (tests); default: the GLB at the URL, with `fetchInit`. */
  hairLoader?: (url: string) => Promise<Object3D>;
}

export interface SetModelOptions {
  /** Play the change effect (default: the stage's `changeEffect`). */
  effect?: boolean;
  /** The wardrobe id this model is (reported by `outfit`). */
  outfit?: string | null;
}

export interface WardrobeStage extends LucinaStage {
  /**
   * Swap the model. `manifest`: a URL or manifest object. Without it, reuse the starting toon
   * look (or the original rig manifest). Resolves once the model is on stage or superseded.
   */
  setModel(url: string, manifest?: string | object | null, opts?: SetModelOptions): Promise<void>;
  /** Swap to a wardrobe outfit by id; rejects for an unknown id. No-op if already wearing it. */
  setOutfit(id: string, opts?: { effect?: boolean }): Promise<void>;
  setWardrobe(entries: WardrobeEntry[]): void;
  readonly wardrobe: readonly WardrobeEntry[];
  /** Outfit on stage now (null: a model that is not from the wardrobe). */
  readonly outfit: string | null;
  readonly modelUrl: string | null;
  /** A swap is loading or playing. */
  readonly changing: boolean;
  /** Called with `true` when a swap starts and `false` when it ends (or fails). */
  onChanging(cb: (changing: boolean) => void): () => void;
  /** Called with the new outfit id after each completed swap. */
  onOutfitChange(cb: (outfit: string | null) => void): () => void;
  /** Put a hairstyle on (`natural`/null: her own hair), with the change effect. */
  setHair(id: string | null, opts?: { effect?: boolean }): Promise<void>;
  /** Tint the hair (a colour id or #rrggbb; null: natural), with the change effect. */
  setHairColor(id: string | null, opts?: { effect?: boolean }): Promise<void>;
  setHairstyles(hairstyles: HairEntry[], colors?: HairColor[]): void;
  readonly hairstyles: readonly HairEntry[];
  readonly hairColors: readonly HairColor[];
  /** Hairstyle on stage now (`natural` for her own hair). */
  readonly hair: string;
  readonly hairColor: string | null;
  /** Called after each completed hair or colour change. */
  onHairChange(cb: (hair: string, color: string | null) => void): () => void;
}

export type StageFactory = (canvas: HTMLCanvasElement, options: LucinaStageOptions) => Promise<LucinaStage>;

function prefersReduced(): boolean {
  try {
    return typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
  } catch {
    return false;
  }
}

/** Where the model is on its canvas (CSS pixels), from its bounding box. */
export function subjectRect(stage: LucinaStage, canvas: HTMLElement): SubjectRect | undefined {
  try {
    const { camera, model } = stage.three;
    const w = canvas.clientWidth || canvas.offsetWidth;
    const h = canvas.clientHeight || canvas.offsetHeight;
    if (!w || !h) return undefined;
    model.updateMatrixWorld(true);
    const box = new Box3().setFromObject(model);
    if (box.isEmpty()) return undefined;
    let x0 = Infinity;
    let y0 = Infinity;
    let x1 = -Infinity;
    let y1 = -Infinity;
    const v = new Vector3();
    for (let i = 0; i < 8; i++) {
      v.set(i & 1 ? box.max.x : box.min.x, i & 2 ? box.max.y : box.min.y, i & 4 ? box.max.z : box.min.z).project(camera);
      const x = ((v.x + 1) / 2) * w;
      const y = ((1 - v.y) / 2) * h;
      x0 = Math.min(x0, x);
      x1 = Math.max(x1, x);
      y0 = Math.min(y0, y);
      y1 = Math.max(y1, y);
    }
    // Clip to the canvas; a bust framing runs off the bottom, which is fine.
    x0 = Math.max(0, x0);
    y0 = Math.max(0, y0);
    x1 = Math.min(w, x1);
    y1 = Math.min(h, y1);
    if (!(x1 - x0 > 4 && y1 - y0 > 4)) return undefined;
    return { x: x0, y: y0, width: x1 - x0, height: y1 - y0 };
  } catch {
    return undefined;
  }
}

function loseContext(canvas: HTMLCanvasElement): void {
  try {
    const gl = (canvas.getContext('webgl2') ?? canvas.getContext('webgl')) as WebGLRenderingContext | null;
    gl?.getExtension('WEBGL_lose_context')?.loseContext();
  } catch {
    /* already gone */
  }
}

const FADE_MS = 240;

export async function createWardrobeStage(
  canvas: HTMLCanvasElement,
  options: WardrobeStageOptions = {},
  factory: StageFactory = createLucinaStage,
): Promise<WardrobeStage> {
  const assetBase = options.assetBase ?? '/v1/assets/';
  let wardrobe: WardrobeEntry[] = parseWardrobe(options.wardrobe ?? []);
  const startEntry = options.outfit ? wardrobe.find((e) => e.id === options.outfit) : undefined;
  const base: LucinaStageOptions = { ...options };
  for (const k of ['wardrobe', 'assetBase', 'outfit', 'changeEffect', 'maxSpeakWait', 'hairstyles', 'hairColors', 'hair', 'hairColor', 'hairLoader'] as const) delete (base as Record<string, unknown>)[k];
  const startUrls = startEntry ? outfitUrls(startEntry, assetBase, options.manifestUrl) : null;

  let cur = await factory(canvas, startUrls ? { ...base, ...startUrls } : base);
  base.toonFallback = toonFallbackFor(cur.three.model, options.manifestUrl) ?? base.toonFallback;
  let curCanvas = canvas;
  let outfit: string | null = startEntry && !cur.info.placeholder ? startEntry.id : null;
  let modelUrl: string | null = cur.info.placeholder ? null : (startUrls?.modelUrl ?? options.modelUrl ?? null);

  // Settings to carry over to the next stage.
  let framing: Framing = options.framing ?? 'bust';
  let reducedOverride: boolean | null = options.reducedMotion ?? null;
  const envOpts = typeof options.environment === 'object' ? options.environment : {};
  let quality: StageQualitySetting | undefined = envOpts.quality;
  let theme: StageTheme | undefined = envOpts.theme;
  let look = false;
  let expression: { e: Expression | null; w: number } | null = null;
  let stateClips: Partial<Record<StageState, string[]>> | null = options.stateClips ?? null;

  const stateCbs = new Set<(next: StageState, prev: StageState) => void>();
  const tapCbs = new Set<(e: TapEvent) => void>();
  const changingCbs = new Set<(c: boolean) => void>();
  const outfitCbs = new Set<(o: string | null) => void>();
  const owned = new Set<HTMLCanvasElement>();
  let disposed = false;
  let changing = 0;
  let swapToken = 0;
  let chain: Promise<void> = Promise.resolve();
  let target: string | null = outfit;

  function bind(s: LucinaStage): () => void {
    const a = s.onStateChange((n, p) => stateCbs.forEach((cb) => cb(n, p)));
    const b = s.onTap((e) => tapCbs.forEach((cb) => cb(e)));
    return () => {
      a();
      b();
    };
  }
  let unbind = bind(cur);

  // ---- Hair ----
  let hairstyles: HairEntry[] = parseHairstyles(options.hairstyles ?? []);
  let hairColors: HairColor[] = parseHairColors(options.hairColors ?? []);
  let hair: string = NATURAL_HAIR;
  let hairColor: string | null = null;
  const hairCbs = new Set<(h: string, c: string | null) => void>();
  const loadHair = options.hairLoader ?? ((url: string) => loadHairPart(url, options.fetchInit ? { fetchInit: options.fetchInit } : {}));
  const rigs = new WeakMap<LucinaStage, HairRig>();
  const assetDir = assetBase.endsWith('/') ? assetBase : `${assetBase}/`;

  function rigFor(s: LucinaStage): HairRig {
    let r = rigs.get(s);
    if (!r) {
      r = new HairRig(s.three.model, s.info.rig?.bones?.head);
      rigs.set(s, r);
    }
    return r;
  }

  const hexOf = (id: string | null): string | null => (id ? (hairColors.find((c) => c.id === id)?.hex ?? (/^#[0-9a-f]{6}$/i.test(id) ? id : null)) : null);

  async function partFor(id: string, model: Object3D): Promise<{ entry: HairEntry | null; part: Object3D | null }> {
    if (id === NATURAL_HAIR) return { entry: null, part: null };
    const entry = hairstyles.find((h) => h.id === id);
    if (!entry?.glb) throw new Error(`Unknown hairstyle: ${id}`);
    const part = await loadHair(assetDir + entry.glb);
    await applyToonHairPart(model, part);
    return { entry, part };
  }

  /** Put the current hair and colour on `s` (a new model); a failed part leaves her own hair. */
  async function dressHair(s: LucinaStage): Promise<void> {
    if (s.info.placeholder) return;
    try {
      const { entry, part } = await partFor(hair, s.three.model);
      rigFor(s).attach(part, entry);
    } catch (e) {
      console.warn('[lucina-stage] hairstyle not loaded:', e);
    }
    rigFor(s).setTint(hexOf(hairColor));
  }

  if (options.hair || options.hairColor) {
    const h = options.hair;
    hair = h && (h === NATURAL_HAIR || hairstyles.some((e) => e.id === h)) ? h : NATURAL_HAIR;
    hairColor = options.hairColor && hairColors.some((c) => c.id === options.hairColor) ? options.hairColor : null;
    await dressHair(cur);
  }

  function setChanging(delta: 1 | -1): void {
    const was = changing > 0;
    changing = Math.max(0, changing + delta);
    if (was !== changing > 0) changingCbs.forEach((cb) => cb(changing > 0));
  }

  function reduced(): boolean {
    return reducedOverride ?? prefersReduced();
  }

  function place(c: HTMLCanvasElement): void {
    const w = canvas.clientWidth || canvas.offsetWidth;
    const h = canvas.clientHeight || canvas.offsetHeight;
    Object.assign(c.style, {
      position: 'absolute',
      left: `${canvas.offsetLeft}px`,
      top: `${canvas.offsetTop}px`,
      width: `${w}px`,
      height: `${h}px`,
    });
  }
  let ro: ResizeObserver | null = null;
  if (typeof ResizeObserver !== 'undefined') {
    ro = new ResizeObserver(() => owned.forEach(place));
    ro.observe(canvas);
  }

  function makeCanvas(): HTMLCanvasElement {
    const parent = canvas.parentElement;
    if (!parent) throw new Error('The stage canvas is not in the document');
    const c = canvas.ownerDocument.createElement('canvas');
    c.className = canvas.className;
    for (const a of ['role', 'aria-label']) {
      const v = canvas.getAttribute(a);
      if (v !== null) c.setAttribute(a, v);
    }
    c.dataset.lucinaOutfit = 'next';
    place(c);
    Object.assign(c.style, { display: 'block', opacity: '0', animation: 'none', pointerEvents: 'none', transition: `opacity ${FADE_MS}ms ease` });
    parent.insertBefore(c, (owned.size ? [...owned].at(-1)! : canvas).nextSibling);
    owned.add(c);
    return c;
  }

  function dropCanvas(c: HTMLCanvasElement): void {
    loseContext(c);
    if (owned.delete(c)) c.remove();
    else Object.assign(c.style, { visibility: 'hidden', pointerEvents: 'none' }); // the host's canvas keeps the layout
  }

  async function waitQuiet(s: LucinaStage, token: number): Promise<void> {
    const until = Date.now() + (options.maxSpeakWait ?? 20_000);
    while (s.speaking && Date.now() < until && token === swapToken && !disposed) {
      await new Promise((r) => setTimeout(r, 150));
    }
  }

  async function doSwap(token: number, url: string, manifest: string | object | null | undefined, o: SetModelOptions): Promise<void> {
    if (token !== swapToken || disposed) return;
    if (url === modelUrl) {
      if (o.outfit !== undefined && o.outfit !== outfit) {
        outfit = o.outfit;
        outfitCbs.forEach((cb) => cb(outfit));
      }
      return;
    }
    setChanging(1);
    let revoke: string | null = null;
    let manifestUrl: string | undefined;
    if (typeof manifest === 'string') manifestUrl = manifest;
    else if (manifest && typeof manifest === 'object') {
      revoke = URL.createObjectURL(new Blob([JSON.stringify(manifest)], { type: 'application/json' }));
      manifestUrl = revoke;
    } else manifestUrl = options.manifestUrl;
    let next: HTMLCanvasElement | null = null;
    let ns: LucinaStage | null = null;
    try {
      next = makeCanvas();
      ns = await factory(next, {
        ...base,
        modelUrl: url,
        manifestUrl,
        placeholder: false,
        fallbackToPlaceholder: false,
        framing,
        reducedMotion: reducedOverride ?? undefined,
        muted: cur.isMuted(),
        environment: options.environment ? { ...envOpts, ...(quality ? { quality } : {}), ...(theme ? { theme } : {}) } : options.environment,
        stateClips: stateClips ?? undefined,
      });
    } catch (e) {
      if (next) dropCanvas(next);
      setChanging(-1);
      throw e;
    } finally {
      if (revoke) URL.revokeObjectURL(revoke);
    }
    const abandon = (): void => {
      ns!.dispose();
      dropCanvas(next!);
      setChanging(-1);
    };
    if (token !== swapToken || disposed) return abandon();
    await dressHair(ns);
    if (token !== swapToken || disposed) return abandon();
    ns.setMuted(cur.isMuted());
    ns.lookAtPointer(look);
    if (expression) ns.setExpression(expression.e, expression.w);
    await waitQuiet(cur, token);
    if (token !== swapToken || disposed) return abandon();
    if (cur.state !== 'speaking') ns.setState(cur.state);

    const old = cur;
    const oldCanvas = curCanvas;
    const incoming = ns;
    const incomingCanvas = next;
    const swap = (): void => {
      unbind();
      cur = incoming;
      curCanvas = incomingCanvas;
      unbind = bind(incoming);
      incomingCanvas.style.opacity = '1';
      incomingCanvas.style.pointerEvents = '';
      delete incomingCanvas.dataset.lucinaOutfit;
      oldCanvas.style.pointerEvents = 'none';
      if (options.environment) {
        // An opaque stage set: the old canvas stays under the new one while it fades in (no dip
        // to the page in between), then hides.
        setTimeout(() => (oldCanvas.style.visibility = 'hidden'), FADE_MS);
      } else {
        // A transparent canvas: crossfade, so the two outfits never show on top of each other.
        Object.assign(oldCanvas.style, { transition: `opacity ${FADE_MS}ms ease`, animation: 'none', opacity: '0' });
      }
    };
    const effect = (o.effect ?? options.changeEffect ?? true) && !reduced();
    if (effect) {
      await playOutfitEffect({
        host: canvas,
        after: incomingCanvas,
        subject: subjectRect(old, oldCanvas),
        theme: theme ?? 'dark',
        reducedMotion: false,
        onPeak: swap,
      }).done;
    } else {
      swap();
      await new Promise((r) => setTimeout(r, FADE_MS));
    }
    rigs.get(old)?.dispose();
    old.dispose();
    dropCanvas(oldCanvas);
    modelUrl = url;
    outfit = o.outfit === undefined ? null : o.outfit;
    if (effect && !disposed) incoming.gesture('happy');
    setChanging(-1);
    outfitCbs.forEach((cb) => cb(outfit));
  }

  /** Run `apply` under the change effect (at once in reduced motion or without the effect). */
  async function underEffect(apply: () => void, effect?: boolean): Promise<void> {
    if (!((effect ?? options.changeEffect ?? true) && !reduced())) {
      apply();
      return;
    }
    await playOutfitEffect({
      host: canvas,
      after: curCanvas,
      subject: subjectRect(cur, curCanvas),
      theme: theme ?? 'dark',
      reducedMotion: false,
      onPeak: apply,
    }).done;
  }

  function queue(job: () => Promise<void>): Promise<void> {
    if (disposed) return Promise.reject(new Error('The stage is disposed'));
    const run = chain.catch(() => {}).then(job);
    chain = run;
    return run;
  }

  function setHair(id: string | null, o: { effect?: boolean } = {}): Promise<void> {
    const want = id ?? NATURAL_HAIR;
    if (want !== NATURAL_HAIR && !hairstyles.some((h) => h.id === want)) return Promise.reject(new Error(`Unknown hairstyle: ${want}`));
    return queue(async () => {
      if (disposed || want === hair) return;
      setChanging(1);
      try {
        const { entry, part } = await partFor(want, cur.three.model);
        if (disposed) return;
        const s = cur;
        await underEffect(() => {
          rigFor(s).attach(part, entry);
          rigFor(s).setTint(hexOf(hairColor));
        }, o.effect);
        hair = want;
        hairCbs.forEach((cb) => cb(hair, hairColor));
      } finally {
        setChanging(-1);
      }
    });
  }

  function setHairColor(id: string | null, o: { effect?: boolean } = {}): Promise<void> {
    if (id !== null && !/^#[0-9a-f]{6}$/i.test(id) && !hairColors.some((c) => c.id === id)) return Promise.reject(new Error(`Unknown hair colour: ${id}`));
    return queue(async () => {
      if (disposed || id === hairColor) return;
      setChanging(1);
      try {
        const s = cur;
        await underEffect(() => rigFor(s).setTint(hexOf(id)), o.effect);
        hairColor = id;
        hairCbs.forEach((cb) => cb(hair, hairColor));
      } finally {
        setChanging(-1);
      }
    });
  }

  function setModel(url: string, manifest?: string | object | null, o: SetModelOptions = {}): Promise<void> {
    if (disposed) return Promise.reject(new Error('The stage is disposed'));
    const token = ++swapToken;
    const run = chain.catch(() => {}).then(() => doSwap(token, url, manifest, o));
    chain = run;
    return run;
  }

  const stage: WardrobeStage = {
    get state() {
      return cur.state;
    },
    setState: (s) => cur.setState(s),
    onStateChange(cb) {
      stateCbs.add(cb);
      return () => stateCbs.delete(cb);
    },
    speak: (audio, opts) => cur.speak(audio, opts),
    stopSpeaking: () => cur.stopSpeaking(),
    get speaking() {
      return cur.speaking;
    },
    setMuted: (m) => cur.setMuted(m),
    isMuted: () => cur.isMuted(),
    setExpression(e, w = 1) {
      expression = e ? { e, w } : null;
      cur.setExpression(e, w);
    },
    onTap(cb) {
      tapCbs.add(cb);
      return () => tapCbs.delete(cb);
    },
    lookAtPointer(enabled) {
      look = !!enabled;
      cur.lookAtPointer(enabled);
    },
    nod: (s) => cur.nod(s),
    setFraming(f) {
      cur.setFraming(f);
      framing = f;
    },
    setReducedMotion(r) {
      reducedOverride = r;
      cur.setReducedMotion(r);
    },
    unlockAudio: () => cur.unlockAudio(),
    resize: () => {
      owned.forEach(place);
      cur.resize();
    },
    play: (clip, opts) => cur.play(clip, opts),
    stopClip: (fade) => cur.stopClip(fade),
    get clip() {
      return cur.clip;
    },
    setStateClips(map) {
      stateClips = { ...(stateClips ?? {}), ...map };
      cur.setStateClips(map);
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      swapToken++;
      unbind();
      ro?.disconnect();
      stateCbs.clear();
      tapCbs.clear();
      changingCbs.clear();
      outfitCbs.clear();
      hairCbs.clear();
      rigs.get(cur)?.dispose();
      cur.dispose();
      for (const c of [...owned]) dropCanvas(c);
      if (curCanvas !== canvas) Object.assign(canvas.style, { visibility: '', opacity: '', pointerEvents: '', transition: '', animation: '' });
    },
    get stats() {
      return cur.stats;
    },
    get info() {
      return cur.info;
    },
    gesture: (name) => cur.gesture(name),
    get gestures() {
      return cur.gestures;
    },
    react: (at) => cur.react(at),
    setQuality(q) {
      quality = q;
      cur.setQuality(q);
    },
    get quality() {
      return cur.quality;
    },
    setTheme(t) {
      theme = t;
      cur.setTheme(t);
    },
    get three() {
      return cur.three;
    },
    setModel,
    setOutfit(id, o = {}) {
      const entry = wardrobe.find((e) => e.id === id);
      if (!entry) return Promise.reject(new Error(`Unknown outfit: ${id}`));
      if (id === target && (changing > 0 || id === outfit)) return chain.catch(() => {});
      target = id;
      const urls = outfitUrls(entry, assetBase, options.manifestUrl);
      const p = setModel(urls.modelUrl, urls.manifestUrl ?? null, { outfit: id, ...(o.effect === undefined ? {} : { effect: o.effect }) });
      p.catch(() => {
        if (target === id) target = outfit;
      });
      return p;
    },
    setWardrobe(entries) {
      wardrobe = parseWardrobe(entries);
    },
    get wardrobe() {
      return wardrobe;
    },
    get outfit() {
      return outfit;
    },
    get modelUrl() {
      return modelUrl;
    },
    get changing() {
      return changing > 0;
    },
    onChanging(cb) {
      changingCbs.add(cb);
      return () => changingCbs.delete(cb);
    },
    onOutfitChange(cb) {
      outfitCbs.add(cb);
      return () => outfitCbs.delete(cb);
    },
    setHair,
    setHairColor,
    setHairstyles(list, colors) {
      hairstyles = parseHairstyles(list);
      if (colors) hairColors = parseHairColors(colors);
    },
    get hairstyles() {
      return hairstyles;
    },
    get hairColors() {
      return hairColors;
    },
    get hair() {
      return hair;
    },
    get hairColor() {
      return hairColor;
    },
    onHairChange(cb) {
      hairCbs.add(cb);
      return () => hairCbs.delete(cb);
    },
  };
  return stage;
}
