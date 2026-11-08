/**
 * The character editor on a stage ("Edit Lucina", docs/design/custom-cast-editor.md §9).
 *
 * - `applyLook(model, look, ctx)` puts a whole look on a loaded model: body sliders (body.ts),
 *   face sliders (face.ts), colours and makeup (paint.ts), accessories (accessories.ts) and outfit
 *   parts (parts.ts). The synchronous half is instant (slider drags); the asynchronous half loads
 *   the GLBs it needs.
 * - `editorPrepareModel(getLook, ctx)` is the stage option `prepareModel`: every model the stage
 *   (or the wardrobe, on an outfit swap) loads gets the current look before her first frame.
 * - `createAvatarEditor(stage, ctx)` is the editing session: live changes, commit/undo/redo,
 *   reset, randomise, camera focus per category, hair and outfit through the wardrobe.
 */
import type { Object3D } from 'three';
import { applyToon, toonFallbackFor, type ToonHandle } from '../toon';
import { fixMaterials, loadModel, type LoadOptions } from '../loader';
import type { LucinaStage } from '../stage';
import type { WardrobeStage } from '../wardrobe';
import { AccessoryRig, parseAccessories, type AccessoryEntry } from './accessories';
import { bodyDeformerFor, type BodyDeformer } from './body';
import { faceMorpherFor, findFaceParts, type FaceMorpher } from './face';
import {
  ACCESSORY_SLOTS,
  CATEGORY_CAMERA,
  cloneLook,
  defaultLook,
  LookHistory,
  looksEqual,
  normalizeLook,
  PART_SLOTS,
  randomLook,
  resetCategory,
  rgbToHsv,
  rng,
  type AccessorySlot,
  type Category,
  type Look,
  type PartSlot,
  type RandomOptions,
} from './look';
import { Painter } from './paint';
import { BASE_OUTFIT, parseParts, PartsRig, resolveParts, type PartEntry } from './parts';

export interface EditorCatalog {
  accessories: AccessoryEntry[];
  parts: PartEntry[];
}

export interface EditorContext extends LoadOptions {
  /** Where `acc-*.glb` and part GLBs live (default `/v1/assets/`). */
  assetBase?: string;
  /** Untrusted lists (wardrobe.json `accessories`, `parts`); validated here. */
  catalog?: { accessories?: unknown; parts?: unknown };
  /** Loads a GLB scene (tests); default: the stage loader with `fetchInit`. */
  loader?: (url: string) => Promise<Object3D>;
}

interface ModelState {
  body: BodyDeformer;
  face: FaceMorpher;
  painter: Painter;
  acc: AccessoryRig;
  parts: PartsRig;
  toons: Map<string, ToonHandle>;
  token: number;
}

const states = new WeakMap<Object3D, ModelState>();

/** Per-model editor state, created on first use (call at load, in the rest pose). */
export function editorStateFor(model: Object3D): ModelState {
  let s = states.get(model);
  if (!s) {
    const face = faceMorpherFor(model);
    s = {
      body: bodyDeformerFor(model),
      face,
      painter: new Painter(model, findFaceParts(model), face.landmarks),
      acc: new AccessoryRig(model),
      parts: new PartsRig(model),
      toons: new Map(),
      token: 0,
    };
    const iris = measureIris(model);
    if (iris) s.painter.setIrisBase(iris);
    states.set(model, s);
  }
  return s;
}

/** The synchronous half of a look: sliders and colours. */
export function applyLookSync(model: Object3D, look: Look): void {
  const s = editorStateFor(model);
  s.body.apply(look);
  s.face.apply(look);
  s.painter.apply(look);
  s.acc.setTiara(look.tiara);
}

const ACC_TOON = {
  preset: 'lit',
  ramp: 'dress_plain',
  rim: 0.3,
  outline: { color: [0.35, 0.35, 0.55], scale: 3.0, albedoMix: 0.45 },
};

/** Toon-shade a prop or part like the model (no-op on standard-material stages). */
async function toonFor(model: Object3D, part: Object3D, materials: Record<string, unknown> = {}): Promise<ToonHandle | null> {
  const source = toonFallbackFor(model);
  if (!source) return null;
  const cfg = source.config as unknown as Record<string, unknown> & { materials: Record<string, unknown>; default?: unknown };
  const dress = cfg.materials.Dress ?? cfg.default;
  const hasMaterials = Object.keys(materials).length > 0;
  return applyToon(part, { ...cfg, materials, default: hasMaterials ? dress : ACC_TOON, hairParts: undefined }, { baseUrl: source.baseUrl });
}

function assetUrl(ctx: EditorContext, name: string): string {
  const base = ctx.assetBase ?? '/v1/assets/';
  return (base.endsWith('/') ? base : `${base}/`) + name;
}

async function loadScene(ctx: EditorContext, name: string): Promise<Object3D> {
  const url = assetUrl(ctx, name);
  if (ctx.loader) return ctx.loader(url);
  const { gltf } = await loadModel(url, ctx.fetchInit ? { fetchInit: ctx.fetchInit } : {});
  fixMaterials(gltf.scene);
  return gltf.scene;
}

/** Applies a whole look: sliders and colours at once, then props and parts (loaded as needed). */
export async function applyLook(model: Object3D, raw: Look, ctx: EditorContext = {}): Promise<void> {
  const look = normalizeLook(raw);
  const s = editorStateFor(model);
  const token = ++s.token;
  applyLookSync(model, look);
  const accessories = parseAccessories(ctx.catalog?.accessories ?? []);
  const parts = parseParts(ctx.catalog?.parts ?? []);
  const jobs: Promise<void>[] = [];
  // Accessories.
  for (const slot of ACCESSORY_SLOTS) {
    const id = look.accessories[slot];
    const entry = id ? accessories.find((a) => a.id === id && a.slot === slot) : undefined;
    if (!entry) {
      s.acc.detach(slot);
      continue;
    }
    if (s.acc.has(slot, entry.id)) continue;
    jobs.push(
      (async () => {
        const scene = await loadScene(ctx, entry.glb);
        const toon = await toonFor(model, scene);
        if (token !== s.token) return toon?.dispose();
        s.toons.get(`acc:${slot}`)?.dispose();
        if (s.acc.attach(entry, scene)) {
          toon?.update();
          if (toon) s.toons.set(`acc:${slot}`, toon);
        } else toon?.dispose();
      })().catch((e) => console.warn(`[lucina-stage] accessory ${entry.id} not loaded:`, e)),
    );
  }
  // Outfit parts (only on a model with body regions: the base).
  if (s.parts.regionNames.length) {
    const want = resolveParts(look.outfit.parts, parts);
    for (const slot of PART_SLOTS) {
      const id = want[slot];
      const entry = id ? parts.find((p) => p.id === id) : undefined;
      if (!entry) {
        if (s.parts.slots[slot]) {
          s.parts.detach(slot);
          s.toons.get(`part:${slot}`)?.dispose();
          s.toons.delete(`part:${slot}`);
        }
        continue;
      }
      if (s.parts.slots[slot] === entry.id) continue;
      jobs.push(
        (async () => {
          const scene = await loadScene(ctx, entry.glb);
          const toon = await toonFor(model, scene, entry.toon?.materials as Record<string, unknown> | undefined);
          if (token !== s.token) return toon?.dispose();
          s.toons.get(`part:${slot}`)?.dispose();
          if (s.parts.attach(entry, scene)) {
            toon?.update();
            if (toon) s.toons.set(`part:${slot}`, toon);
          } else toon?.dispose();
        })().catch((e) => console.warn(`[lucina-stage] part ${entry.id} not loaded:`, e)),
      );
    }
  }
  await Promise.all(jobs);
  if (token === s.token) {
    s.painter.patchAll();
    s.painter.apply(look);
  }
}

/** The `prepareModel` stage option: dress every newly loaded model in the current look. */
export function editorPrepareModel(getLook: () => Look | null | undefined, ctx: EditorContext | (() => EditorContext) = {}): (model: Object3D) => Promise<void> {
  return async (model) => {
    const c = typeof ctx === 'function' ? ctx() : ctx;
    editorStateFor(model); // rest-pose captures happen here, before any animation
    const look = getLook();
    if (look) await applyLook(model, look, c);
  };
}

/** Mean hue/saturation/value of the coloured texels of the iris texture, if it can be read. */
function measureIris(model: Object3D): [number, number, number] | null {
  try {
    let image: CanvasImageSource | null = null;
    model.traverse((o) => {
      const m = (o as { material?: { name?: string; uniforms?: Record<string, { value: { image?: unknown } }> } }).material;
      if (!image && m && !Array.isArray(m) && m.name === 'EyeL') {
        const img = m.uniforms?.albedoMap?.value?.image;
        if (img && typeof (img as { width?: number }).width === 'number') image = img as CanvasImageSource;
      }
    });
    if (!image || typeof document === 'undefined') return null;
    const c = document.createElement('canvas');
    c.width = c.height = 64;
    const g = c.getContext('2d', { willReadFrequently: true });
    if (!g) return null;
    g.drawImage(image, 0, 0, 64, 64);
    const d = g.getImageData(0, 0, 64, 64).data;
    let x = 0, y = 0, sw = 0, ss = 0, sv = 0;
    for (let i = 0; i < d.length; i += 4) {
      const [h, s, v] = rgbToHsv(d[i]! / 255, d[i + 1]! / 255, d[i + 2]! / 255);
      if (s < 0.25 || v < 0.15 || d[i + 3]! < 128) continue;
      const w = s * v;
      x += Math.cos((h * Math.PI) / 180) * w;
      y += Math.sin((h * Math.PI) / 180) * w;
      sw += w;
      ss += s * w;
      sv += v * w;
    }
    if (sw < 1) return null;
    const h = ((Math.atan2(y, x) * 180) / Math.PI + 360) % 360;
    return [h, ss / sw, sv / sw];
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------------------------------------
// The editing session

export interface AvatarEditor {
  readonly look: Look;
  /** Change the look live (no history step; call `commit` when a gesture ends). */
  update(change: (look: Look) => void): void;
  /** Replace the whole look (one history step). */
  load(look: Look): void;
  /** Ends a gesture: one undo step from the last committed look. */
  commit(): void;
  undo(): boolean;
  redo(): boolean;
  readonly canUndo: boolean;
  readonly canRedo: boolean;
  /** Back to the model as authored (one category, or everything). */
  reset(category?: Category): void;
  randomize(opts?: RandomOptions & { seed?: number }): void;
  /** Camera for a category, or for an accessory slot. */
  focus(category: Category, slot?: AccessorySlot): void;
  readonly catalog: EditorCatalog;
  setCatalog(c: { accessories?: unknown; parts?: unknown }): void;
  /** Called after every change (live and committed). Returns an unsubscribe function. */
  onChange(cb: (look: Look, committed: boolean) => void): () => void;
  /** The `prepareModel` option for stages created while this editor is open. */
  prepareModel: (model: Object3D) => Promise<void>;
  /** Resolves when the props, parts, hair and outfit of the current look are on stage. */
  settled(): Promise<void>;
  dispose(): void;
}

function isWardrobe(s: LucinaStage): s is WardrobeStage {
  return typeof (s as WardrobeStage).setOutfit === 'function';
}

export interface EditorOptions extends EditorContext {
  look?: Look | null;
  /** Undo depth (default 50). */
  historySize?: number;
}

export function createAvatarEditor(stage: LucinaStage, opts: EditorOptions = {}): AvatarEditor {
  let look = normalizeLook(opts.look ?? defaultLook());
  let committed = cloneLook(look);
  const history = new LookHistory(opts.historySize ?? 50);
  let catalog: EditorCatalog = {
    accessories: parseAccessories(opts.catalog?.accessories ?? []),
    parts: parseParts(opts.catalog?.parts ?? []),
  };
  const ctx = (): EditorContext => ({ ...opts, catalog });
  const listeners = new Set<(l: Look, c: boolean) => void>();
  let pending: Promise<void> = Promise.resolve();
  let disposed = false;
  const offs: (() => void)[] = [];

  const model = () => stage.three.model;
  const emit = (c: boolean) => listeners.forEach((cb) => cb(cloneLook(look), c));

  /** Hair and outfit go through the wardrobe (they swap models / parts with its machinery). */
  async function wardrobeSide(l: Look): Promise<void> {
    if (!isWardrobe(stage)) return;
    const jobs: Promise<unknown>[] = [];
    const wantOutfit = Object.keys(l.outfit.parts).length && stage.wardrobe.some((w) => w.id === BASE_OUTFIT) ? BASE_OUTFIT : l.outfit.id;
    if (wantOutfit && wantOutfit !== stage.outfit && stage.wardrobe.some((w) => w.id === wantOutfit)) {
      jobs.push(stage.setOutfit(wantOutfit, { effect: false }).catch((e) => console.warn('[lucina-stage] outfit:', e)));
    }
    if (l.hair.style && l.hair.style !== stage.hair) jobs.push(stage.setHair(l.hair.style, { effect: false }).catch((e) => console.warn('[lucina-stage] hair:', e)));
    if (l.hair.color !== undefined && (l.hair.color ?? null) !== stage.hairColor && (l.hair.color || stage.hairColor?.startsWith('#'))) {
      jobs.push(stage.setHairColor(l.hair.color, { effect: false }).catch((e) => console.warn('[lucina-stage] hair colour:', e)));
    }
    await Promise.all(jobs);
  }

  let fittedBody = '';
  function applyNow(): void {
    if (disposed) return;
    const l = cloneLook(look);
    applyLookSync(model(), l);
    const body = JSON.stringify(l.body);
    if (body !== fittedBody) {
      fittedBody = body;
      stage.refit?.();
    }
    pending = pending
      .catch(() => {})
      .then(async () => {
        await wardrobeSide(l);
        if (!disposed) await applyLook(model(), cloneLook(look), ctx());
      });
  }

  if (isWardrobe(stage)) {
    // A swap's new model got the look in prepareModel; hair changes need the ring colour again.
    offs.push(
      stage.onHairChange(() => {
        const s = editorStateFor(model());
        s.painter.patchAll();
        s.painter.apply(look);
      }),
    );
  }
  applyNow();

  const editor: AvatarEditor = {
    get look() {
      return cloneLook(look);
    },
    update(change) {
      const next = cloneLook(look);
      change(next);
      look = normalizeLook(next);
      applyNow();
      emit(false);
    },
    load(l) {
      look = normalizeLook(l);
      applyNow();
      editor.commit();
    },
    commit() {
      if (!looksEqual(look, committed)) {
        history.record(committed);
        committed = cloneLook(look);
      }
      emit(true);
    },
    undo() {
      if (!looksEqual(look, committed)) editor.commit();
      const prev = history.undo(look);
      if (!prev) return false;
      look = prev;
      committed = cloneLook(look);
      applyNow();
      emit(true);
      return true;
    },
    redo() {
      const next = history.redo(look);
      if (!next) return false;
      look = next;
      committed = cloneLook(look);
      applyNow();
      emit(true);
      return true;
    },
    get canUndo() {
      return history.canUndo || !looksEqual(look, committed);
    },
    get canRedo() {
      return history.canRedo;
    },
    reset(category) {
      look = resetCategory(look, category ?? 'looks');
      applyNow();
      editor.commit();
    },
    randomize(o = {}) {
      const accessories: Partial<Record<AccessorySlot, string[]>> = {};
      for (const a of catalog.accessories) (accessories[a.slot] ??= []).push(a.id);
      const hairstyles = isWardrobe(stage) ? stage.hairstyles.map((h) => h.id) : [];
      look = randomLook(look, rng(o.seed ?? (Date.now() ^ (Math.random() * 1e9))), { accessories, hairstyles, ...o });
      applyNow();
      editor.commit();
    },
    focus(category, slot) {
      if (slot === 'back') return stage.setView({ yaw: Math.PI * 0.85, pitch: 0.05, zoom: 0.25, panX: 0, panY: 0 });
      if (slot === 'wristL' || slot === 'wristR') return stage.setView({ yaw: slot === 'wristL' ? 0.6 : -0.6, pitch: 0, zoom: 0.2, panX: 0, panY: -0.25 });
      if (slot === 'neck') return stage.setView({ yaw: 0, pitch: 0, zoom: 0.6, panX: 0, panY: -0.05 });
      if (slot && ['head', 'hair', 'ears', 'eyes'].includes(slot)) return stage.setView({ yaw: slot === 'ears' ? 0.7 : 0.25, pitch: 0.05, zoom: 0.85, panX: 0, panY: 0.02 });
      const cam = CATEGORY_CAMERA[category];
      stage.setView({ yaw: category === 'hair' ? 0.45 : 0, pitch: 0, panX: 0, panY: 0, zoom: cam === 'face' ? 1 : cam === 'bust' ? 0.5 : 0 });
    },
    get catalog() {
      return catalog;
    },
    setCatalog(c) {
      catalog = {
        accessories: c.accessories ? parseAccessories(c.accessories) : catalog.accessories,
        parts: c.parts ? parseParts(c.parts) : catalog.parts,
      };
      applyNow();
    },
    onChange(cb) {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    prepareModel: editorPrepareModel(() => look, ctx),
    settled() {
      return pending.catch(() => {});
    },
    dispose() {
      disposed = true;
      listeners.clear();
      offs.forEach((f) => f());
    },
  };
  return editor;
}

export type { PartSlot };
