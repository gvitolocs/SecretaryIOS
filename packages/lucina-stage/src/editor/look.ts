/**
 * Lucina's look: everything the character editor changes, as one JSON-safe value.
 *
 * Sliders use Custom Cast's convention (docs/design/custom-cast-editor.md): integers 0..100,
 * 50 = the model as authored. Each slider maps to a factor between `min` (at 0) and `max` (at 100)
 * through 1.0 at 50, piecewise linear, so the two sides may differ (a bust shrinks more than it
 * grows). Colours are `#rrggbb` or null (natural). Everything from outside (server, storage) goes
 * through `normalizeLook`, which never throws and clamps every value.
 */

export const LOOK_VERSION = 1;

export type Lang = 'it' | 'en' | 'da';
export type Label = Record<Lang, string>;
export type CameraFocus = 'full' | 'bust' | 'face';

export interface SliderDef {
  id: string;
  label: Label;
  /** Factor at 0 and at 100 (1.0 at 50). */
  min: number;
  max: number;
  /** UI span (0..100) the randomiser stays in: what Custom Cast's shipped characters use, narrowed. */
  taste: [number, number];
  camera: CameraFocus;
}

const L = (it: string, en: string, da: string): Label => ({ it, en, da });

export const BODY_SLIDERS: readonly SliderDef[] = [
  { id: 'height', label: L('Altezza', 'Height', 'Højde'), min: 0.92, max: 1.08, taste: [30, 70], camera: 'full' },
  { id: 'head', label: L('Testa', 'Head size', 'Hovedstørrelse'), min: 0.88, max: 1.12, taste: [38, 64], camera: 'bust' },
  { id: 'neckLength', label: L('Collo: lunghezza', 'Neck length', 'Halslængde'), min: 0.8, max: 1.25, taste: [35, 65], camera: 'bust' },
  { id: 'neckWidth', label: L('Collo: larghezza', 'Neck width', 'Halsbredde'), min: 0.85, max: 1.2, taste: [35, 62], camera: 'bust' },
  { id: 'shoulders', label: L('Spalle', 'Shoulders', 'Skuldre'), min: 0.85, max: 1.15, taste: [30, 62], camera: 'bust' },
  { id: 'bust', label: L('Seno', 'Bust', 'Barm'), min: 0.75, max: 1.35, taste: [30, 72], camera: 'bust' },
  { id: 'waist', label: L('Vita', 'Waist', 'Talje'), min: 0.85, max: 1.12, taste: [30, 60], camera: 'full' },
  { id: 'hips', label: L('Fianchi', 'Hips', 'Hofter'), min: 0.88, max: 1.15, taste: [38, 70], camera: 'full' },
  { id: 'armLength', label: L('Braccia: lunghezza', 'Arm length', 'Armlængde'), min: 0.9, max: 1.1, taste: [38, 62], camera: 'full' },
  { id: 'armThickness', label: L('Braccia: spessore', 'Arm thickness', 'Armtykkelse'), min: 0.85, max: 1.2, taste: [35, 60], camera: 'full' },
  { id: 'legLength', label: L('Gambe: lunghezza', 'Leg length', 'Benlængde'), min: 0.9, max: 1.1, taste: [40, 68], camera: 'full' },
  { id: 'legThickness', label: L('Gambe: spessore', 'Leg thickness', 'Bentykkelse'), min: 0.85, max: 1.2, taste: [35, 62], camera: 'full' },
  { id: 'handSize', label: L('Mani', 'Hand size', 'Håndstørrelse'), min: 0.85, max: 1.15, taste: [40, 60], camera: 'full' },
];

/** Face sliders: the factor is the signed strength of a generated morph (-1 at 0, +1 at 100). */
export const FACE_SLIDERS: readonly SliderDef[] = [
  { id: 'eyeSize', label: L('Occhi: grandezza', 'Eye size', 'Øjenstørrelse'), min: -1, max: 1, taste: [30, 75], camera: 'face' },
  { id: 'eyeWidth', label: L('Occhi: larghezza', 'Eye width', 'Øjenbredde'), min: -1, max: 1, taste: [35, 65], camera: 'face' },
  { id: 'eyeHeight', label: L('Occhi: altezza', 'Eye height', 'Øjenhøjde'), min: -1, max: 1, taste: [35, 65], camera: 'face' },
  { id: 'eyeSpacing', label: L('Occhi: distanza', 'Eye spacing', 'Øjenafstand'), min: -1, max: 1, taste: [35, 65], camera: 'face' },
  { id: 'eyeY', label: L('Occhi: posizione', 'Eye position', 'Øjenposition'), min: -1, max: 1, taste: [35, 65], camera: 'face' },
  { id: 'eyeTilt', label: L('Occhi: inclinazione', 'Eye tilt', 'Øjenhældning'), min: -1, max: 1, taste: [30, 70], camera: 'face' },
  { id: 'browY', label: L('Sopracciglia: altezza', 'Brow height', 'Brynhøjde'), min: -1, max: 1, taste: [30, 70], camera: 'face' },
  { id: 'browAngle', label: L('Sopracciglia: angolo', 'Brow angle', 'Brynvinkel'), min: -1, max: 1, taste: [30, 70], camera: 'face' },
  { id: 'mouthWidth', label: L('Bocca: larghezza', 'Mouth width', 'Mundbredde'), min: -1, max: 1, taste: [35, 65], camera: 'face' },
  { id: 'mouthY', label: L('Bocca: posizione', 'Mouth position', 'Mundposition'), min: -1, max: 1, taste: [35, 65], camera: 'face' },
  { id: 'faceWidth', label: L('Viso: larghezza', 'Face width', 'Ansigtsbredde'), min: -1, max: 1, taste: [30, 62], camera: 'face' },
  { id: 'jaw', label: L('Mento', 'Chin', 'Hage'), min: -1, max: 1, taste: [35, 65], camera: 'face' },
  { id: 'cheeks', label: L('Guance', 'Cheeks', 'Kinder'), min: -1, max: 1, taste: [35, 68], camera: 'face' },
  { id: 'noseY', label: L('Naso: posizione', 'Nose position', 'Næseposition'), min: -1, max: 1, taste: [35, 65], camera: 'face' },
];

export const MAKEUP_LAYERS = ['lipstick', 'eyeliner', 'eyeshadow', 'blush'] as const;
export type MakeupLayer = (typeof MAKEUP_LAYERS)[number];

export const ACCESSORY_SLOTS = ['head', 'hair', 'ears', 'eyes', 'neck', 'back', 'wristL', 'wristR'] as const;
export type AccessorySlot = (typeof ACCESSORY_SLOTS)[number];

/** Outfit part slots (docs/design/avatar-parts.md); a onepiece occupies top and bottom. */
export const PART_SLOTS = ['top', 'bottom', 'onepiece', 'legwear', 'shoes', 'gloves'] as const;
export type PartSlot = (typeof PART_SLOTS)[number];

export interface MakeupValue {
  color: string;
  /** 0..100; 0 = not worn. */
  amount: number;
}

export interface Look {
  v: typeof LOOK_VERSION;
  body: Record<string, number>;
  face: Record<string, number>;
  eyes: { iris: string | null; highlight: boolean };
  skin: { tone: string | null };
  makeup: Record<MakeupLayer, MakeupValue>;
  /** `style`/`color` null: whatever the wardrobe says (rules, pins). */
  hair: { style: string | null; color: string | null; highlight: string | null };
  accessories: Partial<Record<AccessorySlot, string>>;
  /** Her own tiara (part of the base model). */
  tiara: boolean;
  /** `id` null: the wardrobe's outfit. `parts` replace slots of it (outfit mix and match). */
  outfit: { id: string | null; parts: Partial<Record<PartSlot, string>> };
}

export const DEFAULT_MAKEUP: Record<MakeupLayer, MakeupValue> = {
  lipstick: { color: '#d0445e', amount: 0 },
  eyeliner: { color: '#2a1f2e', amount: 0 },
  eyeshadow: { color: '#9a7bc4', amount: 0 },
  blush: { color: '#f08aa0', amount: 0 },
};

export function defaultLook(): Look {
  return {
    v: LOOK_VERSION,
    body: Object.fromEntries(BODY_SLIDERS.map((s) => [s.id, 50])),
    face: Object.fromEntries(FACE_SLIDERS.map((s) => [s.id, 50])),
    eyes: { iris: null, highlight: true },
    skin: { tone: null },
    makeup: cloneMakeup(DEFAULT_MAKEUP),
    hair: { style: null, color: null, highlight: null },
    accessories: {},
    tiara: true,
    outfit: { id: null, parts: {} },
  };
}

function cloneMakeup(m: Record<MakeupLayer, MakeupValue>): Record<MakeupLayer, MakeupValue> {
  return Object.fromEntries(MAKEUP_LAYERS.map((k) => [k, { ...m[k] }])) as Record<MakeupLayer, MakeupValue>;
}

export const HEX_RE = /^#[0-9a-f]{6}$/;
export const ID_RE = /^[a-z0-9][a-z0-9_-]{0,39}$/;

const obj = (v: unknown): Record<string, unknown> => (v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : {});
const hexOr = <T>(v: unknown, fallback: T): string | T => (typeof v === 'string' && HEX_RE.test(v.toLowerCase()) ? v.toLowerCase() : fallback);
const idOr = <T>(v: unknown, fallback: T): string | T => (typeof v === 'string' && ID_RE.test(v) ? v : fallback);

/** 0..100 integer, or the fallback for anything that is not a finite number. */
export function sliderValue(v: unknown, fallback = 50): number {
  return typeof v === 'number' && Number.isFinite(v) ? Math.round(Math.min(100, Math.max(0, v))) : fallback;
}

/** A valid look from untrusted input: unknown keys dropped, values clamped, the rest defaulted. */
export function normalizeLook(raw: unknown): Look {
  const o = obj(raw);
  const look = defaultLook();
  const body = obj(o.body);
  for (const s of BODY_SLIDERS) look.body[s.id] = sliderValue(body[s.id]);
  const face = obj(o.face);
  for (const s of FACE_SLIDERS) look.face[s.id] = sliderValue(face[s.id]);
  const eyes = obj(o.eyes);
  look.eyes = { iris: hexOr(eyes.iris, null), highlight: eyes.highlight !== false };
  look.skin = { tone: hexOr(obj(o.skin).tone, null) };
  const makeup = obj(o.makeup);
  for (const k of MAKEUP_LAYERS) {
    const m = obj(makeup[k]);
    look.makeup[k] = { color: hexOr(m.color, DEFAULT_MAKEUP[k].color), amount: sliderValue(m.amount, 0) };
  }
  const hair = obj(o.hair);
  look.hair = { style: idOr(hair.style, null), color: hexOr(hair.color, null), highlight: hexOr(hair.highlight, null) };
  const acc = obj(o.accessories);
  for (const slot of ACCESSORY_SLOTS) {
    const id = idOr(acc[slot], null);
    if (id) look.accessories[slot] = id;
  }
  look.tiara = o.tiara !== false;
  const outfit = obj(o.outfit);
  const parts = obj(outfit.parts);
  look.outfit = { id: idOr(outfit.id, null), parts: {} };
  for (const slot of PART_SLOTS) {
    const id = idOr(parts[slot], null);
    if (id) look.outfit.parts[slot] = id;
  }
  return look;
}

export function cloneLook(l: Look): Look {
  return {
    v: LOOK_VERSION,
    body: { ...l.body },
    face: { ...l.face },
    eyes: { ...l.eyes },
    skin: { ...l.skin },
    makeup: cloneMakeup(l.makeup),
    hair: { ...l.hair },
    accessories: { ...l.accessories },
    tiara: l.tiara,
    outfit: { id: l.outfit.id, parts: { ...l.outfit.parts } },
  };
}

export function looksEqual(a: Look, b: Look): boolean {
  return JSON.stringify(normalizeLook(a)) === JSON.stringify(normalizeLook(b));
}

/** Slider (0..100) → factor: `min` at 0, 1 (or 0 for signed face sliders) at 50, `max` at 100. */
export function sliderFactor(def: Pick<SliderDef, 'min' | 'max'>, value: number): number {
  const v = sliderValue(value);
  const mid = def.min < 0 ? 0 : 1;
  return v >= 50 ? mid + ((def.max - mid) * (v - 50)) / 50 : mid - ((mid - def.min) * (50 - v)) / 50;
}

export function bodyFactor(look: Look, id: string): number {
  const def = BODY_SLIDERS.find((s) => s.id === id);
  return def ? sliderFactor(def, look.body[id] ?? 50) : 1;
}

export function faceWeight(look: Look, id: string): number {
  const def = FACE_SLIDERS.find((s) => s.id === id);
  return def ? sliderFactor(def, look.face[id] ?? 50) : 0;
}

// ---------------------------------------------------------------------------------------------
// Categories, reset and randomise

export const CATEGORIES = ['looks', 'body', 'face', 'eyes', 'skin', 'hair', 'accessories', 'outfit'] as const;
export type Category = (typeof CATEGORIES)[number];

export const CATEGORY_CAMERA: Record<Category, CameraFocus> = {
  looks: 'full',
  body: 'full',
  face: 'face',
  eyes: 'face',
  skin: 'face',
  hair: 'bust',
  accessories: 'bust',
  outfit: 'full',
};

/** `look` with one category back to the model as authored (`looks`: everything). */
export function resetCategory(look: Look, cat: Category): Look {
  const d = defaultLook();
  const out = cloneLook(look);
  switch (cat) {
    case 'looks':
      return d;
    case 'body':
      out.body = d.body;
      break;
    case 'face':
      out.face = d.face;
      break;
    case 'eyes':
      out.eyes = d.eyes;
      break;
    case 'skin':
      out.skin = d.skin;
      out.makeup = d.makeup;
      break;
    case 'hair':
      out.hair = d.hair;
      break;
    case 'accessories':
      out.accessories = {};
      out.tiara = true;
      break;
    case 'outfit':
      out.outfit = d.outfit;
      break;
  }
  return out;
}

/** A small seeded PRNG (mulberry32), so randomise is testable. */
export function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Curated swatches: natural-looking hair, iris and skin colours, and makeup shades. */
export const SWATCHES = {
  hair: ['#2b3a67', '#1c1c24', '#3b2a22', '#6b4a32', '#a8743f', '#d9b779', '#c9c4d6', '#7a2e3a', '#d27a9a', '#4b6fb5', '#5a8f7b', '#f2f0ea'],
  highlight: ['#9fb0e0', '#e6d2ff', '#ffffff', '#ffd9a8', '#b8f0e0', '#ffc2d6'],
  iris: ['#3d6fd1', '#4aa3df', '#5b8c5a', '#7b5a3c', '#8a4fd6', '#d24a6b', '#c9a227', '#6b7280', '#2e8b8b'],
  skin: ['#fbe8dc', '#f6dccb', '#f0cfb6', '#e2b48f', '#c98f66', '#a46a45', '#7a4b33'],
  lipstick: ['#d0445e', '#b5294a', '#e46a7c', '#c2185b', '#a8432f', '#e88b8b', '#8e2d55'],
  eyeliner: ['#2a1f2e', '#3a2a22', '#1f2a44', '#4b2a5a'],
  eyeshadow: ['#9a7bc4', '#c48a7b', '#7b9ac4', '#d6a1b6', '#b59a6b', '#6b8f7b'],
  blush: ['#f08aa0', '#f4a28c', '#e87b9a', '#f7b0a8'],
} as const;

export interface RandomOptions {
  /** Categories to change (default: body, face, eyes, skin, hair, accessories). */
  categories?: Iterable<Category>;
  /** Accessory ids per slot that may be picked. */
  accessories?: Partial<Record<AccessorySlot, string[]>>;
  /** Hairstyle ids that may be picked (none: keep). */
  hairstyles?: string[];
  /** Chance per accessory slot (default 0.3). */
  accessoryChance?: number;
}

const pick = <T>(r: () => number, list: readonly T[]): T => list[Math.floor(r() * list.length) % list.length]!;

/** Triangular around 50, inside the slider's taste span. */
function tasteValue(r: () => number, def: SliderDef): number {
  const [lo, hi] = def.taste;
  const t = (r() + r()) / 2; // triangular on 0..1, peak at 0.5
  const v = t < 0.5 ? 50 - (50 - lo) * (1 - 2 * t) : 50 + (hi - 50) * (2 * t - 1);
  return Math.round(Math.min(hi, Math.max(lo, v)));
}

/** A random look "within taste" (see the design doc); categories not listed keep `base`. */
export function randomLook(base: Look, random: () => number, opts: RandomOptions = {}): Look {
  const cats = new Set<Category>(opts.categories ?? ['body', 'face', 'eyes', 'skin', 'hair', 'accessories']);
  const out = cloneLook(base);
  if (cats.has('body')) for (const s of BODY_SLIDERS) out.body[s.id] = tasteValue(random, s);
  if (cats.has('face')) for (const s of FACE_SLIDERS) out.face[s.id] = tasteValue(random, s);
  if (cats.has('eyes')) out.eyes = { iris: random() < 0.25 ? null : pick(random, SWATCHES.iris), highlight: true };
  if (cats.has('skin')) {
    out.skin = { tone: random() < 0.5 ? null : pick(random, SWATCHES.skin.slice(0, 5)) };
    for (const k of MAKEUP_LAYERS) {
      const on = random() < (k === 'blush' ? 0.6 : 0.4);
      out.makeup[k] = { color: pick(random, SWATCHES[k]), amount: on ? Math.round(25 + random() * 45) : 0 };
    }
  }
  if (cats.has('hair')) {
    out.hair = {
      style: opts.hairstyles?.length ? pick(random, opts.hairstyles) : out.hair.style,
      color: random() < 0.3 ? null : pick(random, SWATCHES.hair),
      highlight: random() < 0.5 ? null : pick(random, SWATCHES.highlight),
    };
  }
  if (cats.has('accessories')) {
    const chance = opts.accessoryChance ?? 0.3;
    out.accessories = {};
    for (const slot of ACCESSORY_SLOTS) {
      const ids = opts.accessories?.[slot];
      if (ids?.length && random() < chance) out.accessories[slot] = pick(random, ids);
    }
    if (out.accessories.head) out.tiara = false;
    else out.tiara = random() < 0.8;
  }
  return out;
}

// ---------------------------------------------------------------------------------------------
// Undo / redo

/** Undo/redo of whole looks (Custom Cast's MaidUndo: two bounded stacks). */
export class LookHistory {
  private undoStack: Look[] = [];
  private redoStack: Look[] = [];

  constructor(readonly max = 50) {}

  /** Record `before` as the state an undo goes back to. Clears the redo stack. */
  record(before: Look): void {
    this.undoStack.push(cloneLook(before));
    if (this.undoStack.length > this.max) this.undoStack.shift();
    this.redoStack = [];
  }

  /** The look to show after an undo from `current`, or null. */
  undo(current: Look): Look | null {
    const prev = this.undoStack.pop();
    if (!prev) return null;
    this.redoStack.push(cloneLook(current));
    return prev;
  }

  redo(current: Look): Look | null {
    const next = this.redoStack.pop();
    if (!next) return null;
    this.undoStack.push(cloneLook(current));
    return next;
  }

  get canUndo(): boolean {
    return this.undoStack.length > 0;
  }

  get canRedo(): boolean {
    return this.redoStack.length > 0;
  }

  clear(): void {
    this.undoStack = [];
    this.redoStack = [];
  }
}

// ---------------------------------------------------------------------------------------------
// Colour helpers (sRGB hex ↔ HSV), shared by the paint code and the UIs.

export function hexToRgb(hex: string): [number, number, number] {
  const n = parseInt(hex.slice(1), 16);
  return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255];
}

export function rgbToHex(r: number, g: number, b: number): string {
  const c = (x: number) => Math.round(Math.min(1, Math.max(0, x)) * 255).toString(16).padStart(2, '0');
  return `#${c(r)}${c(g)}${c(b)}`;
}

/** h in 0..360, s and v in 0..1. */
export function rgbToHsv(r: number, g: number, b: number): [number, number, number] {
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const d = max - min;
  let h = 0;
  if (d > 1e-9) {
    if (max === r) h = ((g - b) / d) % 6;
    else if (max === g) h = (b - r) / d + 2;
    else h = (r - g) / d + 4;
    h *= 60;
    if (h < 0) h += 360;
  }
  return [h, max === 0 ? 0 : d / max, max];
}

export function hsvToRgb(h: number, s: number, v: number): [number, number, number] {
  const hh = (((h % 360) + 360) % 360) / 60;
  const c = v * s;
  const x = c * (1 - Math.abs((hh % 2) - 1));
  const m = v - c;
  const [r, g, b] = hh < 1 ? [c, x, 0] : hh < 2 ? [x, c, 0] : hh < 3 ? [0, c, x] : hh < 4 ? [0, x, c] : hh < 5 ? [x, 0, c] : [c, 0, x];
  return [r + m, g + m, b + m];
}

export function hexToHsv(hex: string): [number, number, number] {
  return rgbToHsv(...hexToRgb(hex));
}

export function hsvToHex(h: number, s: number, v: number): string {
  return rgbToHex(...hsvToRgb(h, s, v));
}
