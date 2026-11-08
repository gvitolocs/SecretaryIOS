/**
 * Secondary motion for the avatar (hair, braid, cape, belt, chest, accessories): the manifest's
 * `physics` block (config.ts) bound to the loaded model and driven by the stage loop.
 *
 * Why this design:
 * - VRMC_springBone 1.0 semantics (the de-facto web avatar standard; pixiv's three-vrm runs the
 *   same model) on plain GLB bones, so VRoid/UniVRM-tuned values transfer, and Fire Emblem Engage's
 *   own UTJ SpringBone data (stiffnessForce, dragForce, angle limits) maps onto it directly.
 * - Fixed-length sub-steps with an explicit per-1/60 s drag: identical motion at 30/60/120 Hz.
 * - Teleport reset (Engage's automaticReset: a jump or a fast turn resets the chains), a tail speed
 *   limit (MagicaCloth's movement speed limit), cone limits (Engage angle limits): no explosions.
 * - Cloth links between side-by-side chains (the cape) instead of a full XPBD cloth: the cape is
 *   three bone chains in the game too, and it keeps the cost to a few dozen joints.
 *
 * Tiers: `high` (desktop: 90 Hz steps, 2 collision passes, 2 cloth iterations), `medium` (phones:
 * 60 Hz, 1 and 1), `off` (quality Low: the bones keep their animated pose). Reduced motion keeps
 * the physics but scales the swing down. The stage pauses it with its own loop when hidden.
 */
import { Vector3, type Object3D } from 'three';
import type { StageQualitySetting } from '../scene/quality';
import { matchChain, parsePhysics, resolveVariant, type PhysicsConfig } from './config';
import { SpringSolver } from './solver';

export type PhysicsTier = 'high' | 'medium' | 'off';

export interface PhysicsStats {
  tier: PhysicsTier;
  joints: number;
  chains: number;
  activeChains: number;
  colliders: number;
  /** Joints whose bind-pose tail already touches one of its colliders (held at that distance). */
  restOverlaps: number;
  /** Chains of the config the model has no bones for. */
  skipped: string[];
  /** Sub-steps of the last frame. */
  steps: number;
  /** Physics time of the last frame, its running average and the recent peak (ms). */
  ms: number;
  avgMs: number;
  maxMs: number;
}

export interface PhysicsVariantSelection {
  outfit?: string | null;
  hair?: string | null;
}

/** What the stage exposes as `stage.physics`. */
export interface PhysicsHandle {
  readonly tier: PhysicsTier;
  /** Chains running now (after the outfit/hairstyle variant). */
  readonly chains: readonly string[];
  setQuality(q: StageQualitySetting): void;
  /** Outfit and hairstyle overrides (`physics.outfits[id]`, `physics.hairstyles[id]`). */
  setVariant(v: PhysicsVariantSelection): void;
  /** Snap every chain to the animated pose. */
  reset(): void;
  /** A tap: tails within `radius` of `point` get pushed along `dir`. Returns how many moved. */
  poke(point: { x: number; y: number; z: number }, dir?: { x: number; y: number; z: number }, strength?: number, radius?: number): number;
  stats(): PhysicsStats;
}

export interface PhysicsRigOptions {
  quality?: StageQualitySetting;
  reducedMotion?: boolean;
  variant?: PhysicsVariantSelection;
  /** Phone-class device for `auto` (default: a coarse pointer). */
  phone?: boolean;
}

function isPhone(): boolean {
  try {
    return typeof matchMedia === 'function' && matchMedia('(pointer: coarse)').matches;
  } catch {
    return false;
  }
}

/** The tier a quality setting runs physics at. */
export function physicsTier(q: StageQualitySetting | undefined, phone: boolean): PhysicsTier {
  if (q === 'low') return 'off';
  if (q === 'medium') return 'medium';
  if (q === 'high') return phone ? 'medium' : 'high';
  return phone ? 'medium' : 'high';
}

const _pt = new Vector3();
const _dir = new Vector3();
const UP = new Vector3(0, 1, 0);

export class PhysicsRig implements PhysicsHandle {
  /** Timer for the stats (a test page with a fake `performance.now` can put the real one here). */
  static clock: () => number = () => performance.now();
  private tierNow: PhysicsTier;
  private variant: PhysicsVariantSelection = {};
  private active: string[] = [];
  private reduced = false;
  private last = { steps: 0, ms: 0, avg: 0, max: 0 };
  private restored = false;

  private constructor(
    private readonly cfg: PhysicsConfig,
    private readonly solver: SpringSolver,
    private readonly skipped: string[],
    private readonly phone: boolean,
    opts: PhysicsRigOptions,
  ) {
    this.tierNow = physicsTier(opts.quality, phone);
    solver.resetDistance = cfg.resetDistance;
    solver.resetAngle = cfg.resetAngle;
    this.setReducedMotion(!!opts.reducedMotion);
    this.setVariant(opts.variant ?? {});
    this.applyTier();
  }

  /**
   * Binds the manifest's physics block to `model`, which must still be in its bind pose (call it
   * right after loading, before any clip or procedural pose). Null when there is nothing to run.
   */
  static create(model: Object3D, raw: unknown, opts: PhysicsRigOptions = {}): PhysicsRig | null {
    const cfg = parsePhysics(raw);
    if (!cfg) return null;
    const { solver, skipped } = SpringSolver.bind(model, cfg.chains, cfg.colliders, cfg.colliderGroups, cfg.cloth, { rate: cfg.rate, maxSpeed: cfg.maxSpeed });
    if (!solver.chains.length) {
      console.info('[lucina-stage] physics: no chain matches this model', skipped);
      return null;
    }
    return new PhysicsRig(cfg, solver, skipped, opts.phone ?? isPhone(), opts);
  }

  get tier(): PhysicsTier {
    return this.tierNow;
  }

  get chains(): readonly string[] {
    return this.active;
  }

  setQuality(q: StageQualitySetting): void {
    const next = physicsTier(q, this.phone);
    if (next === this.tierNow) return;
    const wasOff = this.tierNow === 'off';
    this.tierNow = next;
    this.applyTier();
    if (wasOff) this.solver.reset();
  }

  private applyTier(): void {
    if (this.tierNow === 'off') {
      this.solver.restoreAll();
      this.restored = true;
    } else {
      this.solver.tier = this.tierNow;
      this.restored = false;
    }
  }

  setVariant(v: PhysicsVariantSelection): void {
    this.variant = { ...this.variant, ...v };
    const specs = resolveVariant(this.cfg, this.variant.outfit, this.variant.hair);
    const on = new Set(specs.map((s) => s.name));
    this.solver.chains.forEach((c, i) => {
      const spec = specs.find((s) => s.name === c.name);
      if (spec) {
        this.solver.setParams(i, spec);
        this.solver.setColliders(i, spec.colliders, this.cfg.colliderGroups);
      }
      this.solver.setActive(i, on.has(c.name));
    });
    this.active = this.solver.chains.filter((c) => c.active).map((c) => c.name);
    this.solver.reset();
  }

  setReducedMotion(r: boolean): void {
    this.reduced = r;
    this.solver.amplitude = r ? this.cfg.reducedAmplitude : 1;
  }

  reset(): void {
    this.solver.reset();
  }

  /** Drop velocities (the loop resumes after a pause). */
  settle(): void {
    this.solver.settle();
  }

  /** One stage frame. Call after the clips and the procedural layer have posed the skeleton. */
  update(dt: number): void {
    if (this.tierNow === 'off') {
      if (!this.restored) this.applyTier();
      return;
    }
    const t0 = PhysicsRig.clock();
    this.last.steps = this.solver.update(dt);
    const ms = PhysicsRig.clock() - t0;
    this.last.ms = ms;
    this.last.avg = this.last.avg ? this.last.avg * 0.95 + ms * 0.05 : ms;
    this.last.max = Math.max(ms, this.last.max * 0.995);
  }

  /**
   * The camera orbited by `angle` radians about the vertical through `pivot`: the swing she would
   * feel if she had turned the other way, scaled by `orbitInertia` (0: physically exact, none).
   */
  orbit(angle: number, pivot: { x: number; y: number; z: number }): void {
    if (this.tierNow === 'off' || this.reduced || !angle) return;
    const a = Math.max(-0.35, Math.min(0.35, angle)) * this.cfg.orbitInertia;
    if (Math.abs(a) < 1e-5) return;
    this.solver.rotateTails(UP, a, _pt.set(pivot.x, pivot.y, pivot.z));
  }

  poke(point: { x: number; y: number; z: number }, dir?: { x: number; y: number; z: number }, strength = 0.03, radius = 0.22): number {
    if (this.tierNow === 'off') return 0;
    _pt.set(point.x, point.y, point.z);
    _dir.set(dir?.x ?? 0, dir?.y ?? 0, dir?.z ?? -1);
    if (_dir.lengthSq() < 1e-12) _dir.set(0, 0, -1);
    _dir.normalize();
    return this.solver.poke(_pt, _dir, strength, radius);
  }

  stats(): PhysicsStats {
    const s = this.solver.stats;
    return {
      tier: this.tierNow,
      joints: s.joints,
      chains: s.chains,
      activeChains: this.active.length,
      colliders: s.colliders,
      restOverlaps: s.restOverlaps,
      skipped: [...this.skipped],
      steps: this.last.steps,
      ms: this.last.ms,
      avgMs: this.last.avg,
      maxMs: this.last.max,
    };
  }

  /** Bones the physics drives (debug overlays, tests). */
  get bones(): readonly Object3D[] {
    return this.solver.boneList;
  }

  /** The underlying solver (tests and the debug harness). */
  get raw(): SpringSolver {
    return this.solver;
  }
}

export { matchChain };
