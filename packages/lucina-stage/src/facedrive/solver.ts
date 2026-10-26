/**
 * Tracker output → `FaceDrive`. ARKit (`ARFaceAnchor.blendShapes`) and MediaPipe Face Landmarker
 * (`faceBlendshapes`) use the same 52 coefficient names, and both give the head as a 4×4 transform of
 * the face in camera space, so one solver serves iOS and the web.
 *
 * Head pose: the rotation relative to a neutral pose captured when tracking starts (the phone is
 * rarely straight in front of the face), expressed in the neutral face's own axes. Both trackers use
 * the same face axes (+X the face's own left, +Y up, +Z out of the face), which are her axes too, so
 * the delta maps onto her bones directly; mirroring flips yaw, roll and left/right.
 */
import { Euler, Matrix4, Quaternion, Vector3 } from 'three';
import type { FaceDrive } from './drive';

/** Coefficients by name (ARKit / MediaPipe category names), 0..1. */
export type Blendshapes = Readonly<Record<string, number>>;

/** Head rotation of the user, anatomical (radians): yaw > 0 toward their left, pitch > 0 chin down, roll > 0 top toward their right. */
export interface HeadPose {
  yaw: number;
  pitch: number;
  roll: number;
}

export interface SolveOptions {
  /** She mirrors the user like a mirror image (default true): you turn left, she turns to the screen's left. */
  mirror?: boolean;
  /** The tracker's Left/Right coefficients are the image's sides, not the face's (default false). */
  swapSides?: boolean;
}

/** Linear 0..1 ramp between `lo` and `hi`. */
export function ramp(v: number | undefined, lo: number, hi: number): number {
  if (v === undefined || !Number.isFinite(v)) return 0;
  return v <= lo ? 0 : v >= hi ? 1 : (v - lo) / (hi - lo);
}

const EYE_YAW_RANGE = 0.45;
const EYE_PITCH_RANGE = 0.3;

/** Maps blendshape coefficients (and an optional head pose) to a face drive. */
export function faceDriveFromBlendshapes(s: Blendshapes, head?: HeadPose | null, opts: SolveOptions = {}): FaceDrive {
  const mirror = opts.mirror ?? true;
  const g = (name: string): number => {
    const v = s[name];
    return typeof v === 'number' && Number.isFinite(v) ? v : 0;
  };
  // The user's own left and right (anatomical).
  const side = (base: string, left: boolean): number => g(`${base}${left !== !!opts.swapSides ? 'Left' : 'Right'}`);
  const blinkUserL = ramp(side('eyeBlink', true), 0.22, 0.68);
  const blinkUserR = ramp(side('eyeBlink', false), 0.22, 0.68);
  // Gaze toward the user's left: the left eye looks out, the right eye looks in.
  const gazeLeft =
    ((side('eyeLookOut', true) - side('eyeLookIn', true)) + (side('eyeLookIn', false) - side('eyeLookOut', false))) / 2;
  const gazeDown =
    (side('eyeLookDown', true) + side('eyeLookDown', false) - side('eyeLookUp', true) - side('eyeLookUp', false)) / 2;

  const d: FaceDrive = {
    jawOpen: ramp(g('jawOpen'), 0.04, 0.5),
    smile: ramp((g('mouthSmileLeft') + g('mouthSmileRight')) / 2, 0.12, 0.65),
    mouthPucker: ramp(g('mouthPucker'), 0.3, 0.85),
    mouthFunnel: ramp(g('mouthFunnel'), 0.15, 0.6),
    mouthWide: ramp((g('mouthStretchLeft') + g('mouthStretchRight')) / 2, 0.12, 0.55),
    browUp: ramp(Math.max(g('browInnerUp'), (g('browOuterUpLeft') + g('browOuterUpRight')) / 2), 0.15, 0.7),
    browDown: ramp((g('browDownLeft') + g('browDownRight')) / 2, 0.2, 0.7),
    // Mirror: the user's left eye is the one on the screen's left, which is her right eye.
    blinkL: mirror ? blinkUserR : blinkUserL,
    blinkR: mirror ? blinkUserL : blinkUserR,
    eyeYaw: (mirror ? -gazeLeft : gazeLeft) * EYE_YAW_RANGE,
    eyePitch: gazeDown * EYE_PITCH_RANGE,
  };
  if (head) {
    d.yaw = mirror ? -head.yaw : head.yaw;
    d.pitch = head.pitch;
    d.roll = mirror ? -head.roll : head.roll;
  }
  return d;
}

const _m = new Matrix4();
const _q = new Quaternion();
const _p = new Vector3();
const _s = new Vector3();
const _e = new Euler(0, 0, 0, 'YXZ');

/**
 * Head pose relative to a neutral pose. The neutral pose is the average of the first `calibrationFrames`
 * samples after start or `recenter()`; until then `push` returns null.
 */
export class HeadPoseSolver {
  calibrationFrames = 8;
  private neutral: Quaternion | null = null;
  private neutralInv = new Quaternion();
  private acc = new Quaternion(0, 0, 0, 0);
  private n = 0;

  recenter(): void {
    this.neutral = null;
    this.acc.set(0, 0, 0, 0);
    this.n = 0;
  }

  get calibrated(): boolean {
    return this.neutral !== null;
  }

  /** `m`: the face's 4×4 transform in camera space, column-major (three.js / simd / MediaPipe order). */
  push(m: ArrayLike<number>): HeadPose | null {
    if (m.length < 16) return null;
    for (let i = 0; i < 16; i++) if (!Number.isFinite(m[i]!)) return null;
    _m.fromArray(Array.from({ length: 16 }, (_, i) => m[i]!));
    _m.decompose(_p, _q, _s);
    _q.normalize();
    if (!this.neutral) {
      // Average on one hemisphere (q and -q are the same rotation).
      const sign = this.n > 0 && this.acc.x * _q.x + this.acc.y * _q.y + this.acc.z * _q.z + this.acc.w * _q.w < 0 ? -1 : 1;
      this.acc.set(this.acc.x + sign * _q.x, this.acc.y + sign * _q.y, this.acc.z + sign * _q.z, this.acc.w + sign * _q.w);
      this.n++;
      if (this.n < this.calibrationFrames) return null;
      this.neutral = this.acc.clone().normalize();
      this.neutralInv.copy(this.neutral).invert();
    }
    // Delta in the neutral face's axes: neutral⁻¹ · current.
    const d = this.neutralInv.clone().multiply(_q);
    _e.setFromQuaternion(d, 'YXZ');
    return { yaw: _e.y, pitch: _e.x, roll: _e.z };
  }
}

/** Blendshapes + head transform → drive, with calibration state. One per tracking session. */
export class FaceSolver {
  readonly head = new HeadPoseSolver();
  constructor(public options: SolveOptions = {}) {}

  solve(shapes: Blendshapes, matrix?: ArrayLike<number> | null): FaceDrive {
    const pose = matrix ? this.head.push(matrix) : null;
    return faceDriveFromBlendshapes(shapes, pose, this.options);
  }

  recenter(): void {
    this.head.recenter();
  }
}
