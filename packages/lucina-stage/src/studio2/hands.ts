/**
 * Hand shapes (Custom Cast's HandList / FingerManager, our own presets): relaxed, fist, open,
 * peace, point, OK and heart-half, per hand, on the model's finger bones.
 *
 * The curl and spread axes come from the hand itself (palm normal from the finger bases, palm
 * side from the thumb), expressed in each segment's parent frame once, so the presets work on any
 * humanoid rig whatever its bone axes. Presets ease in over ~0.2 s and replace the clip's fingers
 * while set; `null` hands the fingers back to the clips.
 */
import { Quaternion, Vector3, type Object3D } from 'three';
import { FINGERS, type Finger, type Humanoid, type Side } from './bones';
import { restLocals } from './rest';

export const HAND_SHAPES = ['relaxed', 'fist', 'open', 'peace', 'point', 'ok', 'heart'] as const;
export type HandShape = (typeof HAND_SHAPES)[number];

/** Per finger: curl of segments 1-3 (degrees, + toward the palm) and spread (+ away from the middle finger). */
export interface FingerPose {
  curl: [number, number, number];
  spread?: number;
}
export type HandPose = Record<Finger, FingerPose>;

const f = (a: number, b: number, c: number, spread = 0): FingerPose => ({ curl: [a, b, c], spread });
const FIST = f(82, 98, 62);

export const HAND_PRESETS: Record<HandShape, HandPose> = {
  relaxed: { thumb: f(8, 12, 8), index: f(12, 18, 10), middle: f(16, 22, 12), ring: f(20, 26, 14), little: f(24, 30, 16) },
  fist: { thumb: f(38, 42, 30), index: FIST, middle: FIST, ring: FIST, little: FIST },
  open: { thumb: f(-8, 0, 0, 6), index: f(0, 0, 0, 9), middle: f(0, 0, 0, 2), ring: f(0, 0, 0, 7), little: f(0, 0, 0, 12) },
  peace: { thumb: f(42, 46, 30), index: f(0, 0, 0, 10), middle: f(0, 0, 0, 10), ring: FIST, little: FIST },
  point: { thumb: f(34, 38, 26), index: f(0, 0, 0), middle: FIST, ring: FIST, little: FIST },
  ok: { thumb: f(30, 32, 24), index: f(46, 56, 40), middle: f(8, 10, 6, 3), ring: f(10, 12, 8, 6), little: f(12, 14, 10, 10) },
  heart: { thumb: f(0, 12, 10), index: f(30, 46, 34, -3), middle: f(32, 46, 34), ring: f(34, 46, 34, -3), little: f(36, 46, 34, -5) },
};

interface SegmentBinding {
  bone: Object3D;
  rest: Quaternion;
  /** Curl axis in the parent's local frame. */
  curl: Vector3;
  /** Spread axis in the parent's local frame (first segment only), signed: + = away from the middle finger. */
  spread: Vector3 | null;
}

interface HandBinding {
  fingers: Partial<Record<Finger, SegmentBinding[]>>;
  /** Current eased values, flattened [finger][seg] + spread. */
  now: Map<Finger, number[]>;
}

const DEG = Math.PI / 180;
const _q = new Quaternion();
const _s = new Quaternion();

function worldOf(o: Object3D): { p: Vector3; q: Quaternion } {
  o.updateWorldMatrix(true, false);
  return { p: o.getWorldPosition(new Vector3()), q: o.getWorldQuaternion(new Quaternion()) };
}

/** Axis given in world space → the local frame of `parent` (or world without one). */
function toParent(parent: Object3D | null, axis: Vector3): Vector3 {
  if (!parent) return axis.clone();
  return axis.clone().applyQuaternion(worldOf(parent).q.invert()).normalize();
}

/** Builds the finger bindings for one hand; null when the hand has no fingers bound. */
export function bindHand(h: Humanoid, side: Side, rest: Map<Object3D, Quaternion>): HandBinding | null {
  const hand = h[side].hand;
  const fingers = h.fingers[side];
  const mid = fingers.middle?.[0] ?? fingers.index?.[0] ?? fingers.ring?.[0];
  if (!hand || !mid) return null;
  const hp = worldOf(hand).p;
  const forward = worldOf(mid).p.sub(hp).normalize();
  const idx = fingers.index?.[0];
  const lit = fingers.little?.[0] ?? fingers.ring?.[0];
  let across = idx && lit ? worldOf(idx).p.sub(worldOf(lit).p) : new Vector3(0, 0, 1);
  across = across.sub(forward.clone().multiplyScalar(across.dot(forward))).normalize();
  // Palm normal, signed toward the palm: the thumb sits on the palm side.
  const palm = new Vector3().crossVectors(forward, across).normalize();
  const thumb = fingers.thumb;
  const tip = thumb?.length ? worldOf(thumb[thumb.length - 1]!).p : null;
  if (tip && tip.sub(hp).dot(palm) < 0) palm.negate();
  const midBase = worldOf(mid).p;

  const out: HandBinding = { fingers: {}, now: new Map() };
  for (const fname of FINGERS) {
    const segs = fingers[fname];
    if (!segs?.length) continue;
    const list: SegmentBinding[] = [];
    for (let i = 0; i < segs.length; i++) {
      const bone = segs[i]!;
      const p0 = worldOf(bone).p;
      const next = segs[i + 1];
      const prev = i > 0 ? segs[i - 1] : hand;
      let dir = next ? worldOf(next).p.sub(p0) : p0.clone().sub(worldOf(prev!).p);
      if (dir.lengthSq() < 1e-12) dir = forward.clone();
      dir.normalize();
      // Toward the palm (thumbs also across it, toward the little finger).
      const toward = fname === 'thumb' ? palm.clone().add(across.clone().multiplyScalar(-0.6)).normalize() : palm;
      const axisW = new Vector3().crossVectors(dir, toward);
      if (axisW.lengthSq() < 1e-8) axisW.crossVectors(forward, palm);
      axisW.normalize();
      let spread: Vector3 | null = null;
      if (i === 0) {
        // + rotates the finger away from the middle one (middle itself: toward the ring side).
        const lateral = fname === 'middle' ? across.clone().negate() : p0.clone().sub(midBase);
        lateral.sub(forward.clone().multiplyScalar(lateral.dot(forward)));
        const sAxis = palm.clone();
        if (lateral.lengthSq() > 1e-10) {
          // Rotating `dir` by + about sAxis moves it along sAxis × dir; flip if that is not "away".
          if (new Vector3().crossVectors(sAxis, dir).dot(lateral) < 0) sAxis.negate();
        }
        spread = toParent(bone.parent, sAxis);
      }
      list.push({ bone, rest: (rest.get(bone) ?? bone.quaternion).clone(), curl: toParent(bone.parent, axisW), spread });
    }
    out.fingers[fname] = list;
    out.now.set(fname, [0, 0, 0, 0]);
  }
  return out;
}

/** Eases `now` toward the preset and writes the finger bones. Returns false when there is nothing to do. */
export function applyHand(b: HandBinding, pose: HandPose, k: number): void {
  for (const fname of FINGERS) {
    const segs = b.fingers[fname];
    const now = b.now.get(fname);
    if (!segs || !now) continue;
    const want = pose[fname];
    const target = [want.curl[0], want.curl[1], want.curl[2], want.spread ?? 0];
    for (let i = 0; i < 4; i++) now[i] = now[i]! + (target[i]! - now[i]!) * k;
    segs.forEach((s, i) => {
      _q.setFromAxisAngle(s.curl, now[Math.min(i, 2)]! * DEG);
      if (s.spread) _q.multiply(_s.setFromAxisAngle(s.spread, now[3]! * DEG));
      s.bone.quaternion.copy(_q).multiply(s.rest);
    });
  }
}

/** Puts the fingers back to rest (the clips take over from the next change). */
export function releaseHand(b: HandBinding): void {
  for (const fname of FINGERS) {
    for (const s of b.fingers[fname] ?? []) s.bone.quaternion.copy(s.rest);
    b.now.set(fname, [0, 0, 0, 0]);
  }
}

export type HandBindingT = HandBinding;
export { restLocals };
