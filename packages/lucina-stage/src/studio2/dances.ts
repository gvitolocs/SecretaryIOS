/**
 * Procedural full-body dance moves (our own choreography; no mocap). No CC0 or CC-BY dance mocap
 * was found that fits the clip pipeline (Quaternius UAL has one dance, already `dance_loop`;
 * Bandai Namco's dances are CC BY-NC; CMU's terms are custom), so these are authored as curves.
 *
 * Each move samples, at 120 BPM, a hips offset and turn, spine/chest/head turns, arm swings,
 * foot lifts and a hand shape. `applyDance` layers it on the current pose (clip or procedural),
 * keeping the feet planted with two-bone IK, so it works on the web (clips) and on iOS (none).
 */
import { Euler, Quaternion, Vector3, type Object3D } from 'three';
import type { Humanoid, Side } from './bones';
import type { HandShape } from './hands';
import { rotateWorld, solveTwoBone, worldPos, setWorldPosition } from './ik';

export const DANCE_MOVES = ['bounce', 'wave', 'step', 'twist', 'heart', 'mix'] as const;
export type DanceMove = (typeof DANCE_MOVES)[number];
export const DANCE_BPM = 120;

interface Rot {
  x?: number;
  y?: number;
  z?: number;
}
export interface ArmFrame {
  /** Raise sideways (rad, + up), swing forward (rad, + forward), twist. */
  raise: number;
  forward: number;
  /** Elbow bend (rad, + bends the forearm forward/up). */
  elbow: number;
}
export interface DanceFrame {
  /** Hips offset as a fraction of her height (x: her left, y: up, z: forward), turn (rad). */
  hips: { x: number; y: number; z: number; yaw: number; roll: number };
  spine: Rot;
  chest: Rot;
  head: Rot;
  arms: Record<Side, ArmFrame>;
  /** Foot lift (fraction of height) per side. */
  lift: Record<Side, number>;
  hands: HandShape | null;
  /** Optional hand targets near the head (heart pose), as fractions of height from the head. */
  handTargets?: Record<Side, [number, number, number]>;
}

const S = (x: number) => Math.sin(x);
const pulse = (b: number) => Math.pow(Math.abs(Math.sin(Math.PI * b)), 1.5);

function base(): DanceFrame {
  return {
    hips: { x: 0, y: 0, z: 0, yaw: 0, roll: 0 },
    spine: {},
    chest: {},
    head: {},
    arms: { l: { raise: 0, forward: 0, elbow: 0 }, r: { raise: 0, forward: 0, elbow: 0 } },
    lift: { l: 0, r: 0 },
    hands: null,
  };
}

/** One frame of a move at `t` seconds. */
export function sampleDance(move: DanceMove, t: number): DanceFrame {
  const b = (t * DANCE_BPM) / 60; // beats
  if (move === 'mix') {
    const order: DanceMove[] = ['bounce', 'wave', 'step', 'twist', 'heart'];
    const seg = Math.floor(b / 8);
    return sampleDance(order[seg % order.length]!, t);
  }
  const f = base();
  switch (move) {
    case 'bounce': {
      const p = pulse(b);
      f.hips.y = -0.02 * p;
      f.hips.x = 0.012 * S((Math.PI * b) / 2);
      f.hips.roll = 0.04 * S((Math.PI * b) / 2);
      f.spine = { x: 0.04 * p };
      f.head = { x: 0.1 * p, z: -0.05 * S((Math.PI * b) / 2) };
      const alt = S(Math.PI * b);
      f.arms.l = { raise: 0.25, forward: 0.55 + 0.35 * alt, elbow: 1.3 };
      f.arms.r = { raise: 0.25, forward: 0.55 - 0.35 * alt, elbow: 1.3 };
      f.hands = 'fist';
      break;
    }
    case 'wave': {
      const sw = S((Math.PI * b) / 2);
      f.hips.x = 0.025 * sw;
      f.hips.y = -0.012 * pulse(b);
      f.hips.roll = -0.06 * sw;
      f.chest = { z: 0.08 * sw };
      f.head = { z: 0.1 * sw, x: -0.04 };
      f.arms.l = { raise: 2.0 + 0.25 * sw, forward: 0.15, elbow: 0.35 + 0.2 * sw };
      f.arms.r = { raise: 2.0 - 0.25 * sw, forward: 0.15, elbow: 0.35 - 0.2 * sw };
      f.hands = 'open';
      break;
    }
    case 'step': {
      const side = S((Math.PI * b) / 2); // weight: + left, - right
      f.hips.x = 0.03 * side;
      f.hips.y = -0.015 * pulse(b);
      f.hips.yaw = 0.12 * side;
      f.chest = { y: -0.1 * side };
      f.head = { y: 0.12 * side, x: 0.05 * pulse(b) };
      f.lift.l = 0.035 * Math.max(0, -side) ** 2;
      f.lift.r = 0.035 * Math.max(0, side) ** 2;
      f.arms.l = { raise: 0.3, forward: 0.4 + 0.6 * side, elbow: 1.0 };
      f.arms.r = { raise: 0.3, forward: 0.4 - 0.6 * side, elbow: 1.0 };
      f.hands = 'relaxed';
      break;
    }
    case 'twist': {
      const tw = S(Math.PI * b);
      f.hips.yaw = 0.35 * tw;
      f.hips.y = -0.03 - 0.01 * pulse(b);
      f.spine = { y: -0.15 * tw };
      f.chest = { y: -0.15 * tw };
      f.head = { y: -0.05 * tw };
      f.arms.l = { raise: 0.45, forward: 0.65, elbow: 1.5 };
      f.arms.r = { raise: 0.45, forward: 0.65, elbow: 1.5 };
      f.hands = 'fist';
      break;
    }
    case 'heart': {
      const sw = S((Math.PI * b) / 2);
      f.hips.x = 0.02 * sw;
      f.hips.roll = -0.05 * sw;
      f.hips.y = -0.01 * pulse(b);
      f.head = { z: 0.12 * sw, x: -0.06 };
      f.chest = { z: 0.05 * sw };
      f.handTargets = { l: [0.05, 0.125, 0.03], r: [-0.05, 0.125, 0.03] };
      f.hands = 'heart';
      break;
    }
    default:
      break;
  }
  return f;
}

const _e = new Euler(0, 0, 0, 'YXZ');
const _q = new Quaternion();
const _v = new Vector3();

function turn(o: { quaternion: Quaternion } | undefined, r: Rot, w: number): void {
  if (!o || (!r.x && !r.y && !r.z)) return;
  _e.set((r.x ?? 0) * w, (r.y ?? 0) * w, (r.z ?? 0) * w, 'YXZ');
  rotateWorld(o as never, _q.setFromEuler(_e));
}

/**
 * Layers a dance frame on the current pose with weight `w` (0..1). `leftSign`: +1 when her left
 * arm is on +X (character space). Feet stay where the pose had them (lifted by `lift`).
 */
export function applyDance(h: Humanoid, f: DanceFrame, w: number, height: number, leftSign: number): void {
  if (w <= 0 || !h.hips) return;
  const feet: Partial<Record<Side, Vector3>> = {};
  for (const s of ['l', 'r'] as const) if (h[s].foot) feet[s] = worldPos(h[s].foot!).clone();
  const hp = worldPos(h.hips).clone();
  setWorldPosition(h.hips, hp.add(_v.set(f.hips.x * leftSign, f.hips.y, f.hips.z).multiplyScalar(height * w)));
  turn(h.hips, { y: f.hips.yaw, z: f.hips.roll * leftSign }, w);
  turn(h.spine, f.spine, w);
  turn(h.chest, f.chest, w);
  turn(h.head, f.head, w);
  // Legs: back to the planted feet (lifted), knees forward.
  for (const s of ['l', 'r'] as const) {
    const L = h[s];
    const foot = feet[s];
    if (!L.upperLeg || !L.lowerLeg || !L.foot || !foot) continue;
    const target = foot.clone();
    target.y += f.lift[s] * height * w;
    solveTwoBone(L.upperLeg, L.lowerLeg, L.foot, target, new Vector3(0, 0, 1));
  }
  // Arms: swings in character space (left arm on +X raises about +Z).
  for (const s of ['l', 'r'] as const) {
    const A = h[s];
    if (!A.upperArm || !A.lowerArm) continue;
    const sign = s === 'l' ? leftSign : -leftSign;
    if (f.handTargets && A.hand && h.head) {
      const t = f.handTargets[s];
      const head = worldPos(h.head);
      const goal = head.clone().add(_v.set(t[0] * leftSign * height, t[1] * height, t[2] * height));
      const cur = worldPos(A.hand).clone();
      solveTwoBone(A.upperArm, A.lowerArm, A.hand, cur.lerp(goal, w), new Vector3(sign, 0.15, -0.25).normalize());
      continue;
    }
    const a = f.arms[s];
    turn(A.upperArm, { z: sign * a.raise, x: -a.forward }, w);
    turn(A.lowerArm, { x: -a.elbow }, w);
  }
}

/**
 * `applyDance` adds to the current pose, but a clip mixer only rewrites a bone whose value changed
 * (and on iOS there are no clips at all), so a bone it left alone still carries last frame's dance.
 * The layer undoes its own last write on such bones before adding the next frame, so nothing drifts.
 */
export class DanceLayer {
  private readonly written = new Map<Object3D, { qb: Quaternion; qa: Quaternion; pb: Vector3; pa: Vector3 }>();

  private bones(h: Humanoid): Object3D[] {
    const out: (Object3D | undefined)[] = [h.hips, h.spine, h.chest, h.neck, h.head];
    for (const s of ['l', 'r'] as const) out.push(h[s].upperArm, h[s].lowerArm, h[s].hand, h[s].upperLeg, h[s].lowerLeg, h[s].foot);
    return out.filter((b): b is Object3D => !!b);
  }

  /** Puts back what nobody else rewrote since our last frame. */
  restore(): void {
    for (const [b, w] of this.written) {
      if (b.quaternion.equals(w.qa)) b.quaternion.copy(w.qb);
      if (b.position.equals(w.pa)) b.position.copy(w.pb);
    }
    this.written.clear();
  }

  apply(h: Humanoid, f: DanceFrame | null, w: number, height: number, leftSign: number): void {
    this.restore();
    if (!f || w <= 0) return;
    const bones = this.bones(h);
    const before = bones.map((b) => ({ b, q: b.quaternion.clone(), p: b.position.clone() }));
    // Children of moved bones need fresh world matrices before the IK reads them.
    h.hips?.parent?.updateMatrixWorld(true);
    applyDance(h, f, w, height, leftSign);
    for (const { b, q, p } of before) this.written.set(b, { qb: q, qa: b.quaternion.clone(), pb: p, pa: b.position.clone() });
  }
}
