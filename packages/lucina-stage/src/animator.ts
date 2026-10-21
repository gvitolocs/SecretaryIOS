/**
 * Procedural animation: breathing, head drift, weight shift, blinks, look-at, state poses,
 * nods and expressions. Produces engine-agnostic frames (rotation deltas per logical bone and
 * weights per logical morph); `rig.ts` applies them to a three.js model.
 */
import { Spring, damp } from './spring';
import { PoseBlender } from './stateMachine';
import {
  clamp,
  EXPRESSIONS,
  LOGICAL_BONES,
  LOGICAL_MORPHS,
  type Expression,
  type LogicalBone,
  type LogicalMorph,
  type RotDelta,
  type StageState,
  type VisemeWeights,
} from './types';

export interface AnimFrame {
  bones: Record<LogicalBone, RotDelta>;
  morphs: Record<LogicalMorph, number>;
}

export interface AnimatorOptions {
  reducedMotion?: boolean;
  /** Random source in [0, 1); injectable for tests. */
  random?: () => number;
}

/** Look-at limits (radians). */
export const LOOK_LIMITS = {
  targetYaw: 0.75,
  targetPitch: 0.5,
  headYaw: 0.42,
  headPitch: 0.26,
  eyeYaw: 0.32,
  eyePitch: 0.2,
};

const TAU = Math.PI * 2;

function smoothstep(x: number): number {
  const t = clamp(x, 0, 1);
  return t * t * (3 - 2 * t);
}

function emptyFrame(): AnimFrame {
  const bones = {} as Record<LogicalBone, RotDelta>;
  for (const b of LOGICAL_BONES) bones[b] = { x: 0, y: 0, z: 0 };
  const morphs = {} as Record<LogicalMorph, number>;
  for (const m of LOGICAL_MORPHS) morphs[m] = 0;
  return { bones, morphs };
}

export class Animator {
  reducedMotion: boolean;
  private readonly random: () => number;
  private readonly blender = new PoseBlender();
  private readonly frame = emptyFrame();
  private t = 0;

  // Look-at.
  private pointer: { yaw: number; pitch: number } | null = null;
  private readonly headYaw = new Spring();
  private readonly headPitch = new Spring();
  private readonly eyeYaw = new Spring();
  private readonly eyePitch = new Spring();
  private saccade = { yaw: 0, pitch: 0, next: 1 };

  // Nods.
  private readonly stressNod = new Spring();
  private readonly listenNod = new Spring();

  // Blinks.
  private blinkNext: number;
  private blinkAt = -1;
  private blinkDur = 0.2;
  private inDoubleBlink = false;

  // Expressions.
  private expression: Expression | null = null;
  private expressionWeight = 1;
  private readonly expr: Record<Expression, number> = { smile: 0, sad: 0, surprised: 0, angry: 0 };
  private idleSmile = { value: 0, target: 0, next: 0, until: 0 };

  constructor(opts: AnimatorOptions = {}) {
    this.reducedMotion = opts.reducedMotion ?? false;
    this.random = opts.random ?? Math.random;
    this.blinkNext = 1 + this.random() * 2;
    this.idleSmile.next = 8 + this.random() * 10;
  }

  private rand(lo: number, hi: number): number {
    return lo + (hi - lo) * this.random();
  }

  /** Pointer direction relative to the face in radians, or null to look at the user. */
  setPointer(target: { yaw: number; pitch: number } | null): void {
    this.pointer = target
      ? {
          yaw: clamp(target.yaw, -LOOK_LIMITS.targetYaw, LOOK_LIMITS.targetYaw),
          pitch: clamp(target.pitch, -LOOK_LIMITS.targetPitch, LOOK_LIMITS.targetPitch),
        }
      : null;
  }

  setExpression(name: Expression | null, weight = 1): void {
    if (name !== null && !EXPRESSIONS.includes(name)) throw new TypeError(`Unknown expression: ${String(name)}`);
    this.expression = name;
    this.expressionWeight = clamp(weight, 0, 1);
  }

  /** A small acknowledging nod (e.g. when the user's speech is detected while listening). */
  nod(strength = 1): void {
    if (this.reducedMotion) return;
    this.listenNod.kick(1.9 * clamp(strength, 0, 2));
  }

  /** Forces a blink now (used e.g. after a tap). */
  blink(): void {
    if (this.blinkAt < 0) this.startBlink(1);
  }

  private startBlink(slow: number): void {
    this.blinkAt = this.t;
    this.blinkDur = 0.19 * slow * this.rand(0.9, 1.15);
  }

  private blinkValue(pose: { blinkMin: number; blinkMax: number; blinkSlow: number }): number {
    if (this.blinkAt < 0 && this.t >= this.blinkNext) {
      this.startBlink(pose.blinkSlow);
    }
    if (this.blinkAt < 0) return 0;
    const p = (this.t - this.blinkAt) / this.blinkDur;
    // close 30%, hold 15%, open 55% (opening is slower, as in real blinks)
    let v: number;
    if (p < 0.3) v = smoothstep(p / 0.3);
    else if (p < 0.45) v = 1;
    else if (p < 1) v = 1 - smoothstep((p - 0.45) / 0.55);
    else {
      v = 0;
      this.blinkAt = -1;
      if (!this.inDoubleBlink && pose.blinkSlow < 1.5 && this.random() < 0.18) {
        // Sometimes a quick second blink follows.
        this.inDoubleBlink = true;
        this.blinkNext = this.t + 0.08;
      } else {
        this.inDoubleBlink = false;
        this.blinkNext = this.t + this.rand(pose.blinkMin, pose.blinkMax);
      }
    }
    return v;
  }

  /**
   * Advances the animation.
   * @param visemes current smoothed mouth weights (zero when not speaking)
   * @param stress energy-peak strength from the lip-sync (0 when none)
   */
  update(dt: number, state: StageState, visemes: VisemeWeights, stress = 0): AnimFrame {
    dt = clamp(dt, 0, 0.1);
    this.t += dt;
    const t = this.t;
    const pose = this.blender.update(state, dt);
    const f = this.frame;
    for (const b of LOGICAL_BONES) {
      const r = f.bones[b];
      r.x = 0;
      r.y = 0;
      r.z = 0;
    }

    // --- Morphs (also in reduced motion) ---
    f.morphs.blink = this.blinkValue(pose);
    f.morphs.blink_l = 0;
    f.morphs.blink_r = 0;
    f.morphs.mouth_a = visemes.a;
    f.morphs.mouth_i = visemes.i;
    f.morphs.mouth_u = visemes.u;
    f.morphs.mouth_e = visemes.e;
    f.morphs.mouth_o = visemes.o;

    for (const e of EXPRESSIONS) {
      const target = this.expression === e ? this.expressionWeight : 0;
      this.expr[e] = damp(this.expr[e], target, 0.16, dt);
    }
    // Occasional soft smile while idle.
    const s = this.idleSmile;
    if (!this.reducedMotion && pose.smileChance > 0.5 && state === 'idle' && this.expression === null) {
      if (s.target === 0 && t >= s.next) {
        s.target = this.rand(0.2, 0.35);
        s.until = t + this.rand(1.6, 3.2);
      } else if (s.target > 0 && t >= s.until) {
        s.target = 0;
        s.next = t + this.rand(9, 22);
      }
    } else if (s.target > 0) {
      s.target = 0;
      s.next = t + this.rand(6, 14);
    }
    s.value = damp(s.value, s.target, 0.45, dt);
    f.morphs.smile = Math.max(this.expr.smile, s.value);
    f.morphs.sad = this.expr.sad;
    f.morphs.angry = this.expr.angry;
    f.morphs.surprised = this.expr.surprised;
    // The mouth shapes and a strong smile fight each other; let speech win a bit.
    const speech = visemes.a + visemes.i + visemes.u + visemes.e + visemes.o;
    if (speech > 0) f.morphs.smile *= 1 - 0.35 * clamp(speech, 0, 1);

    if (this.reducedMotion) {
      this.headYaw.reset();
      this.headPitch.reset();
      this.eyeYaw.reset();
      this.eyePitch.reset();
      this.stressNod.reset();
      this.listenNod.reset();
      return f;
    }

    // --- Look-at ---
    const usePointer = this.pointer !== null && state !== 'thinking';
    const baseYaw = usePointer ? this.pointer!.yaw : 0;
    const basePitch = usePointer ? this.pointer!.pitch : 0;
    if (t >= this.saccade.next) {
      const amp = state === 'thinking' ? 0.05 : 0.025;
      this.saccade.yaw = this.rand(-amp, amp);
      this.saccade.pitch = this.rand(-amp, amp) * 0.6;
      this.saccade.next = t + this.rand(0.6, 2.2);
    }
    const targetYaw = baseYaw + pose.gazeYaw;
    const targetPitch = basePitch + pose.gazePitch;

    const drift = pose.drift;
    const driftYaw = drift * 0.028 * (Math.sin(TAU * 0.071 * t) + 0.6 * Math.sin(TAU * 0.13 * t + 2.1));
    const driftPitch = drift * 0.018 * (Math.sin(TAU * 0.053 * t + 0.7) + 0.5 * Math.sin(TAU * 0.11 * t + 1.3));
    const driftRoll = drift * 0.016 * Math.sin(TAU * 0.047 * t + 0.4);

    const hy = this.headYaw.step(clamp(targetYaw * 0.6, -LOOK_LIMITS.headYaw, LOOK_LIMITS.headYaw), 0.22, dt);
    const hp = this.headPitch.step(clamp(targetPitch * 0.55, -LOOK_LIMITS.headPitch, LOOK_LIMITS.headPitch), 0.24, dt);
    const ey = this.eyeYaw.step(
      clamp(targetYaw - hy + this.saccade.yaw, -LOOK_LIMITS.eyeYaw, LOOK_LIMITS.eyeYaw),
      0.035,
      dt,
    );
    const ep = this.eyePitch.step(
      clamp(targetPitch - hp + this.saccade.pitch, -LOOK_LIMITS.eyePitch, LOOK_LIMITS.eyePitch),
      0.035,
      dt,
    );

    // --- Nods ---
    if (stress > 0 && state === 'speaking') this.stressNod.kick(1.5 * stress);
    const nod = this.stressNod.step(0, 0.09, dt) + this.listenNod.step(0, 0.15, dt);

    // --- Body ---
    const breath = Math.sin(TAU * 0.25 * t);
    const shiftRoll = 0.012 * Math.sin(TAU * 0.045 * t);
    const shiftYaw = 0.01 * Math.sin(TAU * 0.031 * t + 1);

    const { spine, chest, neck, head, eye_l, eye_r, eyes } = f.bones;
    spine.x = pose.lean * 0.6 - 0.004 * breath;
    spine.y = shiftYaw;
    spine.z = shiftRoll;
    chest.x = pose.lean * 0.4 - 0.011 * breath;
    chest.y = hy * 0.15;

    const headYawTotal = hy * 0.85 + driftYaw;
    const headPitchTotal = hp + driftPitch + pose.headPitch + nod - pose.lean * 0.7 + 0.012 * breath;
    const headRoll = pose.headTilt + driftRoll - shiftRoll * 0.6;
    neck.x = headPitchTotal * 0.4;
    neck.y = headYawTotal * 0.4;
    neck.z = headRoll * 0.4;
    head.x = headPitchTotal * 0.6;
    head.y = headYawTotal * 0.6;
    head.z = headRoll * 0.6;

    for (const e of [eye_l, eye_r, eyes]) {
      e.x = ep;
      e.y = ey;
    }
    return f;
  }
}
