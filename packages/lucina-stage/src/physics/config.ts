/**
 * The `physics` block of an avatar manifest: spring-bone chains, the colliders they hit, cloth
 * links between neighbouring chains, and per-outfit / per-hairstyle overrides.
 *
 * Parameters follow VRMC_springBone 1.0 (stiffness, gravityPower, gravityDir, dragForce,
 * hitRadius; values tuned for VRM mean the same here at 60 Hz), plus:
 * - `maxAngle`: a cone limit (degrees) around the animated rest direction (VRMC_springBone_limit's
 *   cone; Fire Emblem Engage's SpringBone angle limits play the same role);
 * - `end`: the tail of the last bone (a length in metres, or a model-space point);
 * - `inertia`: how much of the whole model's movement the chain feels (MagicaCloth's world inertia);
 * - per-joint values: any numeric parameter may be an array, resampled along the chain (Dynamic
 *   Bone's distribution curves).
 *
 * Colliders are spheres or capsules on bones. Give them in bone-local metres (`offset`/`tail`) or,
 * easier to author, as model-space rest positions (`at`/`to`), converted at bind time.
 *
 * Everything is validated: unknown or malformed entries are dropped, numbers are clamped.
 */
import type { Vec3 } from '../types';

export type Num = number | number[];

export interface ColliderSpec {
  name: string;
  bone: string;
  shape: 'sphere' | 'capsule';
  radius: number;
  /** Bone-local centre (sphere) or start (capsule). */
  offset?: Vec3;
  /** Bone-local end of a capsule. */
  tail?: Vec3;
  /** Model-space rest-pose centre / start (alternative to `offset`). */
  at?: Vec3;
  /** Model-space rest-pose capsule end (alternative to `tail`). */
  to?: Vec3;
}

export interface ChainParams {
  stiffness: Num;
  gravityPower: Num;
  gravityDir: Vec3;
  dragForce: Num;
  hitRadius: Num;
  /** Cone limit in degrees around the animated rest direction (0 or absent: none). */
  maxAngle: Num;
  /** 0..1: share of the model's own movement the chain lags behind (1 = fully physical). */
  inertia: number;
  /** Collider names, or `@group` names from `colliderGroups`. */
  colliders: string[];
}

export interface ChainSpec extends ChainParams {
  name: string;
  /** Free-form group (`hair`, `cape`, `chest`...), matched by `@group` in overrides. */
  group?: string;
  /** Bones that rotate, root first. Bone i's tail is bone i+1. */
  bones: string[];
  /**
   * Tail of the last bone: a bone name (Engage's `*_jnt` end joints), a length (m, along the previous
   * segment or the bone's +Y), or a model-space point. Default: its first child bone, else 7 cm.
   */
  end?: string | number | Vec3;
  /** Lowest quality tier the chain runs on (default `medium`). */
  lod: 'medium' | 'high';
}

export interface ClothSpec {
  name: string;
  /** Chain names, side by side; joint i of each is linked to joint i of the next. */
  chains: string[];
  /** 0..1 strength of the side links per iteration. */
  stiffness: number;
  /** Also link the last chain to the first (a skirt ring). */
  loop: boolean;
}

export interface PhysicsVariant {
  /** Chain names (`*` globs) or `@group` to switch off. */
  disable?: string[];
  /** Chain names or `@group` to switch back on (after `disable`). */
  enable?: string[];
  /** Parameter overrides by chain name, glob or `@group`. */
  chains?: Record<string, Partial<ChainParams>>;
}

export interface PhysicsConfig {
  enabled: boolean;
  /** Simulation rate (Hz): steps are at most 1/rate long (default 60). */
  rate: number;
  /** Speed limit for a tail (m/s), against explosions on violent moves. */
  maxSpeed: number;
  /** Teleport: a root jump (m) or turn (degrees) in one frame resets the chains. */
  resetDistance: number;
  resetAngle: number;
  /** Share of a camera orbit felt as if she turned (0 = physically exact: none). */
  orbitInertia: number;
  /** Swing amplitude under reduced motion (0..1). */
  reducedAmplitude: number;
  colliders: ColliderSpec[];
  colliderGroups: Record<string, string[]>;
  chains: ChainSpec[];
  cloth: ClothSpec[];
  outfits: Record<string, PhysicsVariant>;
  hairstyles: Record<string, PhysicsVariant>;
}

const NAME_RE = /^[\w.\-:@ ]{1,64}$/;
/** Chain selectors in overrides: names, `*` globs, `@group`. */
const PATTERN_RE = /^[\w.\-:@ *]{1,64}$/;
const DEFAULTS: ChainParams = {
  stiffness: 1,
  gravityPower: 0,
  gravityDir: [0, -1, 0],
  dragForce: 0.4,
  hitRadius: 0.02,
  maxAngle: 0,
  inertia: 1,
  colliders: [],
};

function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}

function finite(v: unknown): v is number {
  return typeof v === 'number' && Number.isFinite(v);
}

function vec3(v: unknown): Vec3 | undefined {
  if (Array.isArray(v) && v.length === 3 && v.every(finite)) return [v[0] as number, v[1] as number, v[2] as number];
  return undefined;
}

function num(v: unknown, lo: number, hi: number): Num | undefined {
  if (finite(v)) return clamp(v, lo, hi);
  if (Array.isArray(v) && v.length > 0 && v.length <= 64 && v.every(finite)) return v.map((x) => clamp(x, lo, hi));
  return undefined;
}

function name(v: unknown): string | undefined {
  return typeof v === 'string' && NAME_RE.test(v) ? v : undefined;
}

function names(v: unknown, max = 64): string[] {
  return Array.isArray(v) ? v.map(name).filter((s): s is string => !!s).slice(0, max) : [];
}

function patterns(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((s): s is string => typeof s === 'string' && PATTERN_RE.test(s)).slice(0, 64) : [];
}

/** The numeric parameters and their ranges. */
const RANGES = {
  stiffness: [0, 50],
  gravityPower: [0, 10],
  dragForce: [0, 1],
  hitRadius: [0, 1],
  maxAngle: [0, 180],
} as const;

function params(o: Record<string, unknown>): Partial<ChainParams> {
  const out: Partial<ChainParams> = {};
  for (const [k, [lo, hi]] of Object.entries(RANGES) as [keyof typeof RANGES, readonly [number, number]][]) {
    const v = num(o[k], lo, hi);
    if (v !== undefined) out[k] = v;
  }
  const g = vec3(o.gravityDir);
  if (g && Math.hypot(...g) > 1e-6) {
    const l = Math.hypot(...g);
    out.gravityDir = [g[0] / l, g[1] / l, g[2] / l];
  }
  if (finite(o.inertia)) out.inertia = clamp(o.inertia, 0, 1);
  if (Array.isArray(o.colliders)) out.colliders = names(o.colliders);
  return out;
}

function collider(o: Record<string, unknown>): ColliderSpec | undefined {
  const n = name(o.name);
  const bone = name(o.bone);
  const shape = o.shape === 'capsule' ? 'capsule' : o.shape === 'sphere' || o.shape === undefined ? 'sphere' : undefined;
  if (!n || !bone || !shape || !finite(o.radius) || o.radius <= 0) return undefined;
  const c: ColliderSpec = { name: n, bone, shape, radius: clamp(o.radius, 0.001, 2) };
  for (const k of ['offset', 'tail', 'at', 'to'] as const) {
    const v = vec3(o[k]);
    if (v) c[k] = v;
  }
  return c;
}

function variant(o: unknown): PhysicsVariant | undefined {
  if (!o || typeof o !== 'object' || Array.isArray(o)) return undefined;
  const r = o as Record<string, unknown>;
  const v: PhysicsVariant = {};
  if (Array.isArray(r.disable)) v.disable = patterns(r.disable);
  if (Array.isArray(r.enable)) v.enable = patterns(r.enable);
  if (r.chains && typeof r.chains === 'object' && !Array.isArray(r.chains)) {
    v.chains = {};
    for (const [k, p] of Object.entries(r.chains as Record<string, unknown>)) {
      if (PATTERN_RE.test(k) && p && typeof p === 'object') v.chains[k] = params(p as Record<string, unknown>);
    }
  }
  return v;
}

function variants(o: unknown): Record<string, PhysicsVariant> {
  const out: Record<string, PhysicsVariant> = {};
  if (!o || typeof o !== 'object' || Array.isArray(o)) return out;
  for (const [k, v] of Object.entries(o as Record<string, unknown>)) {
    const p = variant(v);
    if (p && name(k)) out[k] = p;
  }
  return out;
}

/** A validated physics block, or null when the manifest has none (or turns it off). */
export function parsePhysics(raw: unknown): PhysicsConfig | null {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return null;
  const r = raw as Record<string, unknown>;
  if (r.enabled === false) return null;
  const colliders: ColliderSpec[] = [];
  const seenC = new Set<string>();
  for (const c of Array.isArray(r.colliders) ? r.colliders.slice(0, 64) : []) {
    const p = c && typeof c === 'object' ? collider(c as Record<string, unknown>) : undefined;
    if (p && !seenC.has(p.name)) {
      seenC.add(p.name);
      colliders.push(p);
    }
  }
  const colliderGroups: Record<string, string[]> = {};
  if (r.colliderGroups && typeof r.colliderGroups === 'object' && !Array.isArray(r.colliderGroups)) {
    for (const [k, v] of Object.entries(r.colliderGroups as Record<string, unknown>)) if (name(k)) colliderGroups[k] = names(v);
  }
  const defaults = { ...DEFAULTS, ...(r.defaults && typeof r.defaults === 'object' ? params(r.defaults as Record<string, unknown>) : {}) };
  const chains: ChainSpec[] = [];
  const seen = new Set<string>();
  for (const c of Array.isArray(r.chains) ? r.chains.slice(0, 128) : []) {
    if (!c || typeof c !== 'object') continue;
    const o = c as Record<string, unknown>;
    const n = name(o.name);
    const bones = names(o.bones, 32);
    if (!n || seen.has(n) || !bones.length) continue;
    const spec: ChainSpec = { ...defaults, ...params(o), name: n, bones, lod: o.lod === 'high' ? 'high' : 'medium' };
    const g = name(o.group);
    if (g) spec.group = g;
    if (finite(o.end) && o.end > 0) spec.end = clamp(o.end, 0.001, 2);
    else if (vec3(o.end)) spec.end = vec3(o.end);
    else if (name(o.end)) spec.end = o.end as string;
    seen.add(n);
    chains.push(spec);
  }
  if (!chains.length) return null;
  const cloth: ClothSpec[] = [];
  for (const c of Array.isArray(r.cloth) ? r.cloth.slice(0, 16) : []) {
    if (!c || typeof c !== 'object') continue;
    const o = c as Record<string, unknown>;
    const n = name(o.name);
    const list = names(o.chains, 32).filter((x) => seen.has(x));
    if (!n || list.length < 2) continue;
    cloth.push({ name: n, chains: list, stiffness: finite(o.stiffness) ? clamp(o.stiffness, 0, 1) : 0.5, loop: o.loop === true });
  }
  const f = (v: unknown, d: number, lo: number, hi: number): number => (finite(v) ? clamp(v, lo, hi) : d);
  return {
    enabled: true,
    rate: f(r.rate, 60, 20, 240),
    maxSpeed: f(r.maxSpeed, 6, 0.1, 100),
    resetDistance: f(r.resetDistance, 0.5, 0.01, 100),
    resetAngle: f(r.resetAngle, 60, 1, 180),
    orbitInertia: f(r.orbitInertia, 0.35, 0, 2),
    reducedAmplitude: f(r.reducedAmplitude, 0.35, 0, 1),
    colliders,
    colliderGroups,
    chains,
    cloth,
    outfits: variants(r.outfits),
    hairstyles: variants(r.hairstyles),
  };
}

/** Does `pattern` (a name, a `*` glob, or `@group`) select `chain`? */
export function matchChain(pattern: string, chain: { name: string; group?: string }): boolean {
  if (pattern.startsWith('@')) return chain.group === pattern.slice(1);
  if (!pattern.includes('*')) return pattern === chain.name;
  const re = new RegExp(`^${pattern.split('*').map((s) => s.replace(/[.+?^${}()|[\]\\]/g, '\\$&')).join('.*')}$`);
  return re.test(chain.name);
}

/**
 * The chains to run for an outfit and hairstyle: the outfit's variant first, then the hairstyle's
 * (a hairstyle part replaces the hair whatever the outfit). Returns new specs; the config is untouched.
 */
export function resolveVariant(cfg: PhysicsConfig, outfit?: string | null, hair?: string | null): ChainSpec[] {
  const layers = [outfit ? cfg.outfits[outfit] : undefined, hair ? cfg.hairstyles[hair] : undefined].filter((v): v is PhysicsVariant => !!v);
  const on = new Map(cfg.chains.map((c) => [c.name, true]));
  const out = cfg.chains.map((c) => ({ ...c }));
  for (const v of layers) {
    for (const p of v.disable ?? []) for (const c of out) if (matchChain(p, c)) on.set(c.name, false);
    for (const p of v.enable ?? []) for (const c of out) if (matchChain(p, c)) on.set(c.name, true);
    for (const [p, over] of Object.entries(v.chains ?? {})) for (const c of out) if (matchChain(p, c)) Object.assign(c, over);
  }
  return out.filter((c) => on.get(c.name));
}

/** Value of a per-joint parameter at joint `i` of `n` (arrays are resampled linearly). */
export function sample(v: Num, i: number, n: number): number {
  if (typeof v === 'number') return v;
  if (v.length === 1 || n <= 1) return v[0]!;
  const t = (i / (n - 1)) * (v.length - 1);
  const a = Math.floor(t);
  const b = Math.min(a + 1, v.length - 1);
  return v[a]! + (v[b]! - v[a]!) * (t - a);
}
