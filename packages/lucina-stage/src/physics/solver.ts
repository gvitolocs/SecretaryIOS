/**
 * Spring-bone solver: VRMC_springBone 1.0 semantics on plain three.js bones, with the extras a
 * phone needs (the design notes are in rig.ts):
 *
 * - Verlet tails per joint, length constraint, sphere/capsule colliders (pushed out, then the
 *   length is restored), a cone limit around the animated rest direction.
 * - Steps are at most 1/rate long and a frame is split into equal sub-steps, so a 30, 60 or
 *   120 Hz display gets the same motion; the drag is applied per 1/60 s, so values tuned for VRM
 *   at 60 fps mean the same here at any step.
 * - Rows of neighbouring chains can be linked into a cloth (the cape): a chain-by-row sweep with
 *   side distance constraints (MagicaCloth's BoneCloth idea), so the cape does not split.
 * - The rest direction comes from the animated pose: a bone the clips drive swings around its
 *   animated rotation; one they don't swings around its bind rotation.
 * - Colliders are "rest-aware": a tail that already touches a collider in the bind pose (an
 *   authoring overlap) is held at that distance instead of being shoved out every frame.
 *
 * No allocation per frame: flat typed arrays and module scratch objects.
 */
import { Matrix4, Quaternion, Vector3, type Bone, type Object3D } from 'three';
import { sample, type ChainSpec, type ColliderSpec } from './config';

const _v = new Vector3();
const _w = new Vector3();
const _p = new Vector3();
const _s = new Vector3();
const _q = new Quaternion();
const _r = new Quaternion();
const _m = new Matrix4();
const _m2 = new Matrix4();
const _rootQ = new Quaternion();
const _rootP = new Vector3();
const DEG = Math.PI / 180;

export type SolverTier = 'high' | 'medium';

export interface SolverStats {
  joints: number;
  chains: number;
  colliders: number;
  /** Joints whose bind-pose tail overlaps one of its colliders (held, not pushed). */
  restOverlaps: number;
}

interface Chain {
  name: string;
  group?: string;
  start: number;
  count: number;
  lod: 'medium' | 'high';
  active: boolean;
  inertia: number;
  gx: number;
  gy: number;
  gz: number;
  colliders: Int32Array;
}

interface Group {
  chains: number[];
  rows: number;
  /** Cloth: side links (chain a, chain b, row, rest length), and the link strength. */
  links: Float32Array | null;
  stiffness: number;
}

interface Collider {
  bone: Object3D;
  capsule: boolean;
  ax: number;
  ay: number;
  az: number;
  bx: number;
  by: number;
  bz: number;
  radius: number;
}

export interface SolverOptions {
  /** Steps per second at most (default 60). */
  rate?: number;
  /** Tail speed limit (m/s). */
  maxSpeed?: number;
}

export interface BindResult {
  solver: SpringSolver;
  /** Chains that could not be bound (missing bones), for diagnostics. */
  skipped: string[];
}

/** Bones by name, exact first, then case/separator-insensitive. */
function boneLookup(root: Object3D): (name: string) => Object3D | undefined {
  const exact = new Map<string, Object3D>();
  const loose = new Map<string, Object3D>();
  const key = (s: string): string => s.toLowerCase().replace(/[\s_.\-:/]/g, '');
  root.traverse((o) => {
    if (!o.name) return;
    if (!exact.has(o.name)) exact.set(o.name, o);
    const k = key(o.name);
    if (!loose.has(k)) loose.set(k, o);
  });
  return (n) => exact.get(n) ?? exact.get(n.replace(/\s/g, '_').replace(/[[\].:/]/g, '')) ?? loose.get(key(n));
}

function depth(o: Object3D): number {
  let d = 0;
  for (let p = o.parent; p; p = p.parent) d++;
  return d;
}

export class SpringSolver {
  readonly chains: Chain[] = [];
  private readonly groups: Group[] = [];
  private readonly colliders: Collider[] = [];
  private readonly colliderIndex = new Map<string, number>();
  private readonly bones: Object3D[] = [];
  private n = 0;
  // Per joint.
  private localTail!: Float32Array;
  private len!: Float32Array;
  private scale!: Float32Array;
  private stiff!: Float32Array;
  private grav!: Float32Array;
  private drag!: Float32Array;
  private hit!: Float32Array;
  private cosMax!: Float32Array;
  private sinMax!: Float32Array;
  private cur!: Float32Array;
  private prev!: Float32Array;
  private next!: Float32Array;
  private head!: Float32Array;
  private restDir!: Float32Array;
  private baseWorld!: Float32Array;
  private world!: Float32Array;
  private parentQ!: Float32Array;
  private base!: Float32Array;
  private sim!: Float32Array;
  private written!: Float32Array;
  private mats: Matrix4[] = [];
  /** Distance from the bind tail of joint j to the core of collider k (j * C + k). */
  private restDist!: Float32Array;
  private colliderWorld!: Float32Array;
  private rootPrev = new Matrix4();
  private rootInit = false;
  private hPrev = 1 / 60;
  private overlaps = 0;

  rate: number;
  maxSpeed: number;
  tier: SolverTier = 'high';
  /** 0..1 swing amplitude (reduced motion lowers it). */
  amplitude = 1;
  resetDistance = 0.5;
  resetAngle = 60;

  private constructor(
    private readonly root: Object3D,
    opts: SolverOptions,
  ) {
    this.rate = opts.rate ?? 60;
    this.maxSpeed = opts.maxSpeed ?? 6;
  }

  /**
   * Binds chains and colliders to `root` (a model in its bind pose: run before any animation).
   * Model-space collider points and `end` points are relative to `root`.
   */
  static bind(root: Object3D, chains: ChainSpec[], colliders: ColliderSpec[], groups: Record<string, string[]> = {}, cloth: { chains: string[]; stiffness: number; loop: boolean }[] = [], opts: SolverOptions = {}): BindResult {
    const s = new SpringSolver(root, opts);
    const find = boneLookup(root);
    root.updateMatrixWorld(true);
    const modelToWorld = root.matrixWorld;
    const skipped: string[] = [];

    for (const c of colliders) {
      const bone = find(c.bone);
      if (!bone) continue;
      const toLocal = _m.copy(bone.matrixWorld).invert();
      const a = c.offset ? _v.fromArray(c.offset) : c.at ? _v.fromArray(c.at).applyMatrix4(modelToWorld).applyMatrix4(toLocal) : _v.set(0, 0, 0);
      const ax = a.x, ay = a.y, az = a.z;
      const capsule = c.shape === 'capsule' && !!(c.tail || c.to);
      const b = !capsule ? _w.set(ax, ay, az) : c.tail ? _w.fromArray(c.tail) : _w.fromArray(c.to!).applyMatrix4(modelToWorld).applyMatrix4(toLocal);
      bone.matrixWorld.decompose(_p, _q, _s);
      s.colliderIndex.set(c.name, s.colliders.length);
      // Radii are given in metres of the world: store them in bone units.
      s.colliders.push({ bone, capsule, ax, ay, az, bx: b.x, by: b.y, bz: b.z, radius: c.radius / Math.max(_s.x, 1e-6) });
    }

    // Chains: bones must exist and not be simulated twice; children before... sorted by root depth.
    const used = new Set<Object3D>();
    const bound: { spec: ChainSpec; bones: Object3D[] }[] = [];
    for (const spec of chains) {
      const bones: Object3D[] = [];
      for (const n of spec.bones) {
        const b = find(n);
        if (!b || used.has(b)) break;
        if (bones.length && b.parent !== bones[bones.length - 1]) break;
        bones.push(b);
      }
      if (!bones.length) {
        skipped.push(spec.name);
        continue;
      }
      for (const b of bones) used.add(b);
      bound.push({ spec, bones });
    }
    bound.sort((a, b) => depth(a.bones[0]!) - depth(b.bones[0]!));
    const n = bound.reduce((k, c) => k + c.bones.length, 0);
    s.alloc(n);

    let j = 0;
    for (const { spec, bones } of bound) {
      const chain: Chain = {
        name: spec.name,
        group: spec.group,
        start: j,
        count: bones.length,
        lod: spec.lod,
        active: true,
        inertia: spec.inertia,
        gx: spec.gravityDir[0],
        gy: spec.gravityDir[1],
        gz: spec.gravityDir[2],
        colliders: new Int32Array(0),
      };
      for (let i = 0; i < bones.length; i++, j++) {
        const bone = bones[i]!;
        s.bones[j] = bone;
        bone.matrixWorld.decompose(_p, _q, _s);
        s.scale[j] = _s.x;
        // Tail in bone-local space: the next bone, the first child bone, or `end`.
        const named = i + 1 === bones.length && typeof spec.end === 'string' ? find(spec.end) : undefined;
        const child = bones[i + 1] ?? bone.children.find((c) => (c as Bone).isBone);
        const local = _v;
        if (named) local.setFromMatrixPosition(named.matrixWorld).applyMatrix4(_m.copy(bone.matrixWorld).invert());
        else if (child && (i + 1 < bones.length || spec.end === undefined || typeof spec.end === 'string')) local.copy(child.position);
        else if (Array.isArray(spec.end)) local.fromArray(spec.end).applyMatrix4(modelToWorld).applyMatrix4(_m.copy(bone.matrixWorld).invert());
        else {
          const length = (typeof spec.end === 'number' ? spec.end : 0.07) / Math.max(_s.x, 1e-6);
          if (i > 0) {
            // Continue the previous segment's direction.
            _w.copy(bone.position).applyQuaternion(bones[i - 1]!.getWorldQuaternion(_r)).normalize();
            local.copy(_w).applyQuaternion(_r.copy(_q).invert()).setLength(length);
          } else local.set(0, length, 0);
        }
        const l = local.length() || 1e-4;
        s.len[j] = l * _s.x;
        local.divideScalar(l);
        s.localTail.set([local.x, local.y, local.z], j * 3);
        s.base.set([bone.quaternion.x, bone.quaternion.y, bone.quaternion.z, bone.quaternion.w], j * 4);
        s.written.set(s.base.subarray(j * 4, j * 4 + 4), j * 4);
        // Bind tail (world) and its distance to every collider.
        _w.copy(local).multiplyScalar(l).applyMatrix4(bone.matrixWorld);
        s.cur.set([_w.x, _w.y, _w.z], j * 3);
        s.prev.set([_w.x, _w.y, _w.z], j * 3);
      }
      s.chains.push(chain);
      s.setParams(s.chains.length - 1, spec);
      s.setColliders(s.chains.length - 1, spec.colliders, groups);
    }
    s.updateColliders();
    const C = s.colliders.length;
    s.restDist = new Float32Array(Math.max(1, n * C));
    for (let k = 0; k < n; k++) {
      for (let c = 0; c < C; c++) s.restDist[k * C + c] = s.distanceToCore(c, s.cur[k * 3]!, s.cur[k * 3 + 1]!, s.cur[k * 3 + 2]!);
    }
    s.buildGroups(cloth);
    s.countOverlaps();
    // The model may still be moved (turned to face the camera) before the first frame.
    s.needsReset = true;
    return { solver: s, skipped };
  }

  private alloc(n: number): void {
    this.n = n;
    const f = (k: number): Float32Array => new Float32Array(Math.max(1, n * k));
    this.localTail = f(3);
    this.len = f(1);
    this.scale = f(1);
    this.stiff = f(1);
    this.grav = f(1);
    this.drag = f(1);
    this.hit = f(1);
    this.cosMax = f(1);
    this.sinMax = f(1);
    this.cur = f(3);
    this.prev = f(3);
    this.next = f(3);
    this.head = f(3);
    this.restDir = f(3);
    this.baseWorld = f(4);
    this.world = f(4);
    this.parentQ = f(4);
    this.base = f(4);
    this.sim = f(4);
    this.written = f(4);
    this.mats = Array.from({ length: n }, () => new Matrix4());
    this.bones.length = n;
  }

  private buildGroups(cloth: { chains: string[]; stiffness: number; loop: boolean }[]): void {
    const inCloth = new Set<number>();
    const byName = new Map(this.chains.map((c, i) => [c.name, i]));
    const groups: { chains: number[]; stiffness: number; loop: boolean }[] = [];
    for (const cl of cloth) {
      const idx = cl.chains.map((n) => byName.get(n)).filter((i): i is number => i !== undefined && !inCloth.has(i));
      if (idx.length < 2) continue;
      idx.forEach((i) => inCloth.add(i));
      groups.push({ chains: idx, stiffness: cl.stiffness, loop: cl.loop && idx.length > 2 });
    }
    for (let i = 0; i < this.chains.length; i++) if (!inCloth.has(i)) groups.push({ chains: [i], stiffness: 0, loop: false });
    // Shallow roots first: a chain hanging off another chain's bone sees its parent already solved.
    const rootDepth = (g: { chains: number[] }): number => Math.min(...g.chains.map((i) => depth(this.bones[this.chains[i]!.start]!)));
    groups.sort((a, b) => rootDepth(a) - rootDepth(b));
    for (const g of groups) {
      const rows = Math.max(...g.chains.map((i) => this.chains[i]!.count));
      let links: Float32Array | null = null;
      if (g.chains.length > 1) {
        const out: number[] = [];
        const pairs = g.chains.length - (g.loop ? 0 : 1);
        for (let p = 0; p < pairs; p++) {
          const a = g.chains[p]!;
          const b = g.chains[(p + 1) % g.chains.length]!;
          const ca = this.chains[a]!;
          const cb = this.chains[b]!;
          for (let r = 0; r < Math.min(ca.count, cb.count); r++) {
            const ja = ca.start + r;
            const jb = cb.start + r;
            const d = Math.hypot(this.cur[ja * 3]! - this.cur[jb * 3]!, this.cur[ja * 3 + 1]! - this.cur[jb * 3 + 1]!, this.cur[ja * 3 + 2]! - this.cur[jb * 3 + 2]!);
            out.push(a, b, r, d);
          }
        }
        links = new Float32Array(out);
      }
      this.groups.push({ chains: g.chains, rows, links, stiffness: g.stiffness });
    }
  }

  /** Per-joint parameters of chain `ci` from a (resolved) spec. */
  setParams(ci: number, spec: Pick<ChainSpec, 'stiffness' | 'gravityPower' | 'gravityDir' | 'dragForce' | 'hitRadius' | 'maxAngle' | 'inertia'>): void {
    const c = this.chains[ci]!;
    c.inertia = spec.inertia;
    [c.gx, c.gy, c.gz] = spec.gravityDir;
    for (let i = 0; i < c.count; i++) {
      const j = c.start + i;
      this.stiff[j] = sample(spec.stiffness, i, c.count);
      this.grav[j] = sample(spec.gravityPower, i, c.count);
      this.drag[j] = Math.min(sample(spec.dragForce, i, c.count), 1);
      this.hit[j] = sample(spec.hitRadius, i, c.count);
      const a = sample(spec.maxAngle, i, c.count);
      this.cosMax[j] = a > 0 && a < 180 ? Math.cos(a * DEG) : -2;
      this.sinMax[j] = a > 0 && a < 180 ? Math.sin(a * DEG) : 0;
    }
  }

  /** Collider names / `@group`s for chain `ci`. */
  setColliders(ci: number, list: string[], groups: Record<string, string[]>): void {
    const out = new Set<number>();
    const add = (n: string, d = 0): void => {
      if (n.startsWith('@')) {
        if (d < 4) for (const m of groups[n.slice(1)] ?? []) add(m, d + 1);
        return;
      }
      const k = this.colliderIndex.get(n);
      if (k !== undefined) out.add(k);
    };
    list.forEach((n) => add(n));
    this.chains[ci]!.colliders = Int32Array.from(out);
  }

  private countOverlaps(): void {
    let k = 0;
    const C = this.colliders.length;
    for (const c of this.chains) {
      for (let j = c.start; j < c.start + c.count; j++) {
        for (const ci of c.colliders) if (this.restDist[j * C + ci]! < this.colliders[ci]!.radius * this.scale[j]! + this.hit[j]!) k++;
      }
    }
    this.overlaps = k;
  }

  /**
   * Deepest collider penetration of any running tail right now, beyond what the bind pose allows
   * (m; 0 = none). For tests and the render harness.
   */
  maxPenetration(): { depth: number; chain: string; collider: number } {
    let worst = { depth: 0, chain: '', collider: -1 };
    const C = this.colliders.length;
    for (const c of this.chains) {
      if (!this.runs(c)) continue;
      for (let j = c.start; j < c.start + c.count; j++) {
        for (const k of c.colliders) {
          const want = Math.min(this.colliderWorld[k * 7 + 6]! + this.hit[j]!, this.restDist[j * C + k]!);
          const d = want - this.distanceToCore(k, this.cur[j * 3]!, this.cur[j * 3 + 1]!, this.cur[j * 3 + 2]!);
          if (d > worst.depth) worst = { depth: d, chain: c.name, collider: k };
        }
      }
    }
    return worst;
  }

  /** Bind-pose overlaps, for authoring: which tail sits how deep in which collider (m). */
  restOverlapList(): { chain: string; joint: number; collider: string; depth: number }[] {
    const out: { chain: string; joint: number; collider: string; depth: number }[] = [];
    const C = this.colliders.length;
    const names = [...this.colliderIndex.entries()];
    for (const c of this.chains) {
      for (let j = c.start; j < c.start + c.count; j++) {
        for (const ci of c.colliders) {
          const want = this.colliders[ci]!.radius * this.scale[j]! + this.hit[j]!;
          const d = this.restDist[j * C + ci]!;
          if (d < want) out.push({ chain: c.name, joint: j - c.start, collider: names.find(([, k]) => k === ci)![0], depth: +(want - d).toFixed(4) });
        }
      }
    }
    return out;
  }

  get stats(): SolverStats {
    this.countOverlaps();
    return { joints: this.n, chains: this.chains.length, colliders: this.colliders.length, restOverlaps: this.overlaps };
  }

  /** Switch chains on/off by index (inactive chains show their animated/bind pose). */
  setActive(ci: number, on: boolean): void {
    const c = this.chains[ci]!;
    if (c.active === on) return;
    c.active = on;
    if (!on) for (let j = c.start; j < c.start + c.count; j++) this.restore(j);
    else this.needsReset = true;
  }

  private restore(j: number): void {
    const b = this.bones[j]!;
    b.quaternion.set(this.base[j * 4]!, this.base[j * 4 + 1]!, this.base[j * 4 + 2]!, this.base[j * 4 + 3]!);
    this.written.set(this.base.subarray(j * 4, j * 4 + 4), j * 4);
  }

  /** Every bone back to its animated/bind rotation (physics off). */
  restoreAll(): void {
    this.capture();
    for (let j = 0; j < this.n; j++) this.restore(j);
  }

  private runs(c: Chain): boolean {
    return c.active && (c.lod === 'medium' || this.tier === 'high');
  }

  /** Reads the animated rotation of every joint (a clip may drive it) and poses the bones with it. */
  private capture(): void {
    for (let j = 0; j < this.n; j++) {
      const q = this.bones[j]!.quaternion;
      const o = j * 4;
      if (q.x !== this.written[o] || q.y !== this.written[o + 1] || q.z !== this.written[o + 2] || q.w !== this.written[o + 3]) {
        this.base[o] = q.x;
        this.base[o + 1] = q.y;
        this.base[o + 2] = q.z;
        this.base[o + 3] = q.w;
      } else q.set(this.base[o]!, this.base[o + 1]!, this.base[o + 2]!, this.base[o + 3]!);
    }
  }

  private updateColliders(): void {
    const C = this.colliders.length;
    if (!this.colliderWorld || this.colliderWorld.length < C * 7) this.colliderWorld = new Float32Array(Math.max(7, C * 7));
    for (let k = 0; k < C; k++) {
      const c = this.colliders[k]!;
      const e = c.bone.matrixWorld.elements;
      const o = k * 7;
      const w = this.colliderWorld;
      w[o] = e[0]! * c.ax + e[4]! * c.ay + e[8]! * c.az + e[12]!;
      w[o + 1] = e[1]! * c.ax + e[5]! * c.ay + e[9]! * c.az + e[13]!;
      w[o + 2] = e[2]! * c.ax + e[6]! * c.ay + e[10]! * c.az + e[14]!;
      w[o + 3] = e[0]! * c.bx + e[4]! * c.by + e[8]! * c.bz + e[12]!;
      w[o + 4] = e[1]! * c.bx + e[5]! * c.by + e[9]! * c.bz + e[13]!;
      w[o + 5] = e[2]! * c.bx + e[6]! * c.by + e[10]! * c.bz + e[14]!;
      w[o + 6] = c.radius * Math.hypot(e[0]!, e[1]!, e[2]!);
    }
  }

  /** Distance from a point to collider k's core (centre or segment). Writes the closest core point to _p. */
  private distanceToCore(k: number, x: number, y: number, z: number): number {
    const w = this.colliderWorld;
    const o = k * 7;
    let cx = w[o]!;
    let cy = w[o + 1]!;
    let cz = w[o + 2]!;
    if (this.colliders[k]!.capsule) {
      const dx = w[o + 3]! - cx;
      const dy = w[o + 4]! - cy;
      const dz = w[o + 5]! - cz;
      const ll = dx * dx + dy * dy + dz * dz;
      if (ll > 1e-12) {
        let t = ((x - cx) * dx + (y - cy) * dy + (z - cz) * dz) / ll;
        t = t < 0 ? 0 : t > 1 ? 1 : t;
        cx += dx * t;
        cy += dy * t;
        cz += dz * t;
      }
    }
    _p.set(cx, cy, cz);
    return Math.hypot(x - cx, y - cy, z - cz);
  }

  /**
   * Advances the simulation by `dt` seconds (one frame). Call after the animation has posed the
   * skeleton; it updates `root`'s world matrices itself.
   */
  update(dt: number, maxSubsteps = this.tier === 'high' ? 4 : 3): number {
    if (!this.n) return 0;
    this.capture();
    this.root.updateMatrixWorld(true);
    this.updateColliders();
    const teleported = this.checkRoot();
    if (teleported || this.needsReset) {
      this.needsReset = false;
      this.solve(0, 'reset');
      this.output();
      return 0;
    }
    if (!(dt > 0)) {
      this.solve(0, 'pose');
      this.output();
      return 0;
    }
    const hMax = 1 / (this.tier === 'high' ? Math.max(this.rate, 90) : this.rate);
    const steps = Math.min(Math.max(Math.ceil(dt / hMax - 1e-6), 1), maxSubsteps);
    const h = Math.min(dt / steps, 1 / 20);
    for (let s = 0; s < steps; s++) this.solve(h, 'step');
    this.output();
    return steps;
  }

  /** Root jump (teleport) → true; small root moves feed `inertia` < 1 chains. */
  private checkRoot(): boolean {
    const m = this.root.matrixWorld;
    if (!this.rootInit) {
      this.rootPrev.copy(m);
      this.rootInit = true;
      return false;
    }
    _m2.copy(this.rootPrev).invert().premultiply(m); // delta: prev → now
    this.rootPrev.copy(m);
    const e = _m2.elements;
    const moved = Math.hypot(e[12]!, e[13]!, e[14]!);
    _m2.decompose(_rootP, _rootQ, _s);
    const turned = 2 * Math.acos(Math.min(1, Math.abs(_rootQ.w))) / DEG;
    if (moved > this.resetDistance || turned > this.resetAngle) return true;
    if (moved < 1e-7 && turned < 1e-5) return false;
    for (const c of this.chains) {
      const follow = 1 - c.inertia;
      if (follow <= 0) continue;
      for (let j = c.start; j < c.start + c.count; j++) {
        for (const arr of [this.cur, this.prev]) {
          _v.fromArray(arr, j * 3);
          _w.copy(_v).applyMatrix4(_m2);
          _v.lerp(_w, follow);
          arr[j * 3] = _v.x;
          arr[j * 3 + 1] = _v.y;
          arr[j * 3 + 2] = _v.z;
        }
      }
    }
    return false;
  }

  private solve(h: number, mode: 'step' | 'pose' | 'reset'): void {
    for (const g of this.groups) {
      for (let r = 0; r < g.rows; r++) {
        for (const ci of g.chains) {
          const c = this.chains[ci]!;
          if (r < c.count && this.runs(c)) this.predict(c, c.start + r, r, h, mode);
        }
        if (g.links && mode === 'step') this.link(g, r);
        for (const ci of g.chains) {
          const c = this.chains[ci]!;
          if (r < c.count && this.runs(c)) this.finish(c, c.start + r, mode);
        }
      }
    }
    if (mode === 'step') this.hPrev = h;
  }

  /** Head, rest direction and the predicted (Verlet) tail of joint j. */
  private predict(c: Chain, j: number, r: number, h: number, mode: 'step' | 'pose' | 'reset'): void {
    const bone = this.bones[j]!;
    const o3 = j * 3;
    const o4 = j * 4;
    // Parent world transform: the animated parent for a chain root, else the joint just solved.
    if (r === 0) {
      bone.parent!.matrixWorld.decompose(_p, _q, _s);
      _v.copy(bone.position).applyMatrix4(bone.parent!.matrixWorld);
    } else {
      _q.fromArray(this.world, o4 - 4);
      _v.copy(bone.position).applyMatrix4(this.mats[j - 1]!);
    }
    this.parentQ[o4] = _q.x;
    this.parentQ[o4 + 1] = _q.y;
    this.parentQ[o4 + 2] = _q.z;
    this.parentQ[o4 + 3] = _q.w;
    this.head[o3] = _v.x;
    this.head[o3 + 1] = _v.y;
    this.head[o3 + 2] = _v.z;
    _r.fromArray(this.base, o4).premultiply(_q); // base world rotation
    _r.toArray(this.baseWorld, o4);
    _w.fromArray(this.localTail, o3).applyQuaternion(_r); // rest direction
    this.restDir[o3] = _w.x;
    this.restDir[o3 + 1] = _w.y;
    this.restDir[o3 + 2] = _w.z;
    const nx = this.next;
    if (mode === 'reset') {
      const L = this.len[j]!;
      nx[o3] = _v.x + _w.x * L;
      nx[o3 + 1] = _v.y + _w.y * L;
      nx[o3 + 2] = _v.z + _w.z * L;
      return;
    }
    const cx = this.cur[o3]!;
    const cy = this.cur[o3 + 1]!;
    const cz = this.cur[o3 + 2]!;
    if (mode === 'pose') {
      nx[o3] = cx;
      nx[o3 + 1] = cy;
      nx[o3 + 2] = cz;
      return;
    }
    // VRM at 60 Hz: Δ' = Δ(1 - drag) + (stiffness·restDir + gravity·dir)/60. As velocities, for any step h:
    // v' = v (1 - drag)^(60h) + a h, with a = 60 (stiffness·restDir + gravity·dir).
    const damp = Math.pow(1 - this.drag[j]!, 60 * h) / this.hPrev;
    const ks = 60 * this.stiff[j]!;
    const kg = 60 * this.grav[j]!;
    let vx = (cx - this.prev[o3]!) * damp + (_w.x * ks + c.gx * kg) * h;
    let vy = (cy - this.prev[o3 + 1]!) * damp + (_w.y * ks + c.gy * kg) * h;
    let vz = (cz - this.prev[o3 + 2]!) * damp + (_w.z * ks + c.gz * kg) * h;
    const sp = Math.hypot(vx, vy, vz);
    if (sp > this.maxSpeed) {
      const k = this.maxSpeed / sp;
      vx *= k;
      vy *= k;
      vz *= k;
    }
    nx[o3] = cx + vx * h;
    nx[o3 + 1] = cy + vy * h;
    nx[o3 + 2] = cz + vz * h;
  }

  /** Side links of a cloth row (Jacobi-free sequential projection, a few passes). */
  private link(g: Group, r: number): void {
    const L = g.links!;
    const k = g.stiffness * 0.5;
    const iters = this.tier === 'high' ? 2 : 1;
    const nx = this.next;
    for (let it = 0; it < iters; it++) {
      for (let i = 0; i < L.length; i += 4) {
        if (L[i + 2] !== r) continue;
        const ca = this.chains[L[i]!]!;
        const cb = this.chains[L[i + 1]!]!;
        if (!this.runs(ca) || !this.runs(cb)) continue;
        const a = (ca.start + r) * 3;
        const b = (cb.start + r) * 3;
        const dx = nx[b]! - nx[a]!;
        const dy = nx[b + 1]! - nx[a + 1]!;
        const dz = nx[b + 2]! - nx[a + 2]!;
        const d = Math.hypot(dx, dy, dz);
        if (d < 1e-9) continue;
        const f = ((d - L[i + 3]!) / d) * k;
        nx[a] = nx[a]! + dx * f;
        nx[a + 1] = nx[a + 1]! + dy * f;
        nx[a + 2] = nx[a + 2]! + dz * f;
        nx[b] = nx[b]! - dx * f;
        nx[b + 1] = nx[b + 1]! - dy * f;
        nx[b + 2] = nx[b + 2]! - dz * f;
      }
    }
  }

  /** Constraints (length, cone, colliders), then the bone rotation that aims at the tail. */
  private finish(c: Chain, j: number, mode: 'step' | 'pose' | 'reset'): void {
    const o3 = j * 3;
    const o4 = j * 4;
    const hx = this.head[o3]!;
    const hy = this.head[o3 + 1]!;
    const hz = this.head[o3 + 2]!;
    const rx = this.restDir[o3]!;
    const ry = this.restDir[o3 + 1]!;
    const rz = this.restDir[o3 + 2]!;
    const L = this.len[j]!;
    const nx = this.next;
    // Direction head → tail, unit.
    let dx = nx[o3]! - hx;
    let dy = nx[o3 + 1]! - hy;
    let dz = nx[o3 + 2]! - hz;
    let d = Math.hypot(dx, dy, dz);
    if (d < 1e-9) {
      dx = rx;
      dy = ry;
      dz = rz;
      d = 1;
    }
    dx /= d;
    dy /= d;
    dz /= d;
    if (mode !== 'reset') {
      // Cone limit around the rest direction.
      const cm = this.cosMax[j]!;
      const cos = dx * rx + dy * ry + dz * rz;
      if (cm > -1.5 && cos < cm) {
        let px = dx - rx * cos;
        let py = dy - ry * cos;
        let pz = dz - rz * cos;
        let pl = Math.hypot(px, py, pz);
        if (pl < 1e-6) {
          // Exactly opposite: any perpendicular will do.
          px = Math.abs(rx) < 0.9 ? 0 : 1;
          py = Math.abs(rx) < 0.9 ? -rz : 0;
          pz = Math.abs(rx) < 0.9 ? ry : -rx;
          pl = Math.hypot(px, py, pz);
        }
        const sm = this.sinMax[j]!;
        dx = rx * cm + (px / pl) * sm;
        dy = ry * cm + (py / pl) * sm;
        dz = rz * cm + (pz / pl) * sm;
      }
      // Colliders: push the tail out, restore the length; twice on the high tier.
      const cols = c.colliders;
      if (cols.length) {
        const C = this.colliders.length;
        const passes = this.tier === 'high' ? 2 : 1;
        const hit = this.hit[j]!;
        const w = this.colliderWorld;
        for (let pass = 0; pass < passes; pass++) {
          let moved = false;
          let tx = hx + dx * L;
          let ty = hy + dy * L;
          let tz = hz + dz * L;
          for (let i = 0; i < cols.length; i++) {
            const k = cols[i]!;
            const dist = this.distanceToCore(k, tx, ty, tz);
            const want = Math.min(w[k * 7 + 6]! + hit, Math.max(this.restDist[j * C + k]!, 0));
            if (dist >= want) continue;
            if (dist > 1e-9) {
              const s = want / dist;
              tx = _p.x + (tx - _p.x) * s;
              ty = _p.y + (ty - _p.y) * s;
              tz = _p.z + (tz - _p.z) * s;
            } else {
              tx = _p.x + rx * want;
              ty = _p.y + ry * want;
              tz = _p.z + rz * want;
            }
            moved = true;
          }
          if (!moved) break;
          dx = tx - hx;
          dy = ty - hy;
          dz = tz - hz;
          d = Math.hypot(dx, dy, dz) || 1;
          dx /= d;
          dy /= d;
          dz /= d;
        }
      }
    }
    const tx = hx + dx * L;
    const ty = hy + dy * L;
    const tz = hz + dz * L;
    if (mode === 'reset') {
      this.prev[o3] = this.cur[o3] = tx;
      this.prev[o3 + 1] = this.cur[o3 + 1] = ty;
      this.prev[o3 + 2] = this.cur[o3 + 2] = tz;
    } else if (mode === 'step') {
      this.prev[o3] = this.cur[o3]!;
      this.prev[o3 + 1] = this.cur[o3 + 1]!;
      this.prev[o3 + 2] = this.cur[o3 + 2]!;
      this.cur[o3] = tx;
      this.cur[o3 + 1] = ty;
      this.cur[o3 + 2] = tz;
    }
    // World rotation: the base rotation, turned so the rest direction points at the tail.
    _v.set(rx, ry, rz);
    _w.set(dx, dy, dz);
    _q.setFromUnitVectors(_v, _w).multiply(_r.fromArray(this.baseWorld, o4));
    _q.toArray(this.world, o4);
    // Local = parent⁻¹ · world.
    _r.fromArray(this.parentQ, o4).invert().multiply(_q);
    _r.toArray(this.sim, o4);
    _v.set(hx, hy, hz);
    _s.setScalar(this.scale[j]!);
    this.mats[j]!.compose(_v, _q, _s);
    // Children outside the chain (and the renderer) read the bone's own matrix.
    this.bones[j]!.matrixWorld.copy(this.mats[j]!);
  }

  /** Writes the solved rotations to the bones (blended toward the animated pose by `amplitude`). */
  private output(): void {
    const a = this.amplitude;
    for (const c of this.chains) {
      if (!this.runs(c)) continue;
      for (let j = c.start; j < c.start + c.count; j++) {
        const o = j * 4;
        _q.fromArray(this.sim, o);
        if (a < 1) _q.slerp(_r.fromArray(this.base, o), 1 - a);
        this.bones[j]!.quaternion.copy(_q);
        this.written[o] = _q.x;
        this.written[o + 1] = _q.y;
        this.written[o + 2] = _q.z;
        this.written[o + 3] = _q.w;
      }
    }
  }

  /** Snap every tail to the animated pose (teleport, outfit or clip cut), on the next update. */
  reset(): void {
    this.needsReset = true;
  }

  private needsReset = false;

  /** Keep the positions, drop the velocities (resume after a pause). */
  settle(): void {
    this.prev.set(this.cur);
  }

  /** Turns every tail by `angle` (radians) about the world axis through `pivot` (inertia of a turn). */
  rotateTails(axis: Vector3, angle: number, pivot: Vector3): void {
    if (!angle) return;
    _q.setFromAxisAngle(axis, angle * this.amplitude);
    for (const arr of [this.cur, this.prev]) {
      for (let j = 0; j < this.n; j++) {
        _v.fromArray(arr, j * 3).sub(pivot).applyQuaternion(_q).add(pivot);
        _v.toArray(arr, j * 3);
      }
    }
  }

  /** Pushes tails near `point` along `dir` by up to `strength` metres (a velocity kick). */
  poke(point: Vector3, dir: Vector3, strength: number, radius: number): number {
    let hits = 0;
    const s = strength * this.amplitude;
    for (const c of this.chains) {
      if (!this.runs(c)) continue;
      for (let j = c.start; j < c.start + c.count; j++) {
        _v.fromArray(this.cur, j * 3);
        const d = _v.distanceTo(point);
        if (d >= radius) continue;
        _v.addScaledVector(dir, s * (1 - d / radius));
        _v.toArray(this.cur, j * 3);
        hits++;
      }
    }
    return hits;
  }

  /** World tail of joint j (tests, debug). */
  tail(j: number, out = new Vector3()): Vector3 {
    return out.fromArray(this.cur, j * 3);
  }

  /** Index of the first joint of a chain by name. */
  chainIndex(name: string): number {
    return this.chains.findIndex((c) => c.name === name);
  }

  /** Bones driven by the solver. */
  get boneList(): readonly Object3D[] {
    return this.bones;
  }
}
