/**
 * Face drive (Custom Cast's `CMDlibFaceTracking`, F1): a tracked face steers her head, eyes, blinks,
 * mouth and brows. The source (ARKit on iOS, MediaPipe on the web) sends `FaceDrive` samples; this
 * smooths them, eases in when tracking starts and back out to her own procedural motion when it stops
 * or the face is lost, and writes them over the animator's frame before the rig applies it.
 */
import type { AnimFrame } from '../animator';
import { clamp } from '../types';

/**
 * One face sample, in her space (the `RotDelta` convention of the rig, radians; weights 0..1).
 * Mirroring (the user sees her like a mirror image) is the source's job: see `faceDriveFromBlendshapes`.
 * - yaw > 0 turns her head toward +X (her left, screen right for the viewer);
 * - pitch > 0 tips it down; roll > 0 tilts the top toward -X (her right);
 * - blinkL / blinkR close her left / right eye; eyeYaw / eyePitch turn her eyes like yaw / pitch.
 * Every field is optional: a missing channel keeps its last value.
 */
export interface FaceDrive {
  yaw?: number;
  pitch?: number;
  roll?: number;
  jawOpen?: number;
  blinkL?: number;
  blinkR?: number;
  smile?: number;
  mouthPucker?: number;
  mouthFunnel?: number;
  mouthWide?: number;
  browUp?: number;
  browDown?: number;
  eyeYaw?: number;
  eyePitch?: number;
}

export const FACE_CHANNELS = [
  'yaw',
  'pitch',
  'roll',
  'jawOpen',
  'blinkL',
  'blinkR',
  'smile',
  'mouthPucker',
  'mouthFunnel',
  'mouthWide',
  'browUp',
  'browDown',
  'eyeYaw',
  'eyePitch',
] as const satisfies readonly (keyof FaceDrive)[];
export type FaceChannel = (typeof FACE_CHANNELS)[number];

/** Rotation limits (radians); weights are clamped to 0..1. */
export const FACE_LIMITS = { yaw: 0.8, pitch: 0.55, roll: 0.5, eyeYaw: 0.35, eyePitch: 0.25 } as const;

/** Low-pass time constants (s): blinks must stay crisp, the head calm. */
const TAU: Record<FaceChannel, number> = {
  yaw: 0.08,
  pitch: 0.08,
  roll: 0.1,
  jawOpen: 0.04,
  blinkL: 0.025,
  blinkR: 0.025,
  smile: 0.1,
  mouthPucker: 0.06,
  mouthFunnel: 0.06,
  mouthWide: 0.06,
  browUp: 0.1,
  browDown: 0.1,
  eyeYaw: 0.05,
  eyePitch: 0.05,
};

/** Share of the head rotation carried by the neck (the rest by the head bone). */
export const NECK_SHARE = 0.4;

function limit(c: FaceChannel, v: number): number {
  switch (c) {
    case 'yaw':
    case 'pitch':
    case 'roll':
    case 'eyeYaw':
    case 'eyePitch':
      return clamp(v, -FACE_LIMITS[c], FACE_LIMITS[c]);
    default:
      return clamp(v, 0, 1);
  }
}

function zeroes(): Record<FaceChannel, number> {
  const r = {} as Record<FaceChannel, number>;
  for (const c of FACE_CHANNELS) r[c] = 0;
  return r;
}

const mix = (a: number, b: number, w: number): number => a + (b - a) * w;

export class FaceDriver {
  /** Seconds without a sample before she eases back to her own motion (face lost). */
  timeout = 0.5;
  /** Ease in/out time constant (s). */
  fade = 0.22;
  private readonly value = zeroes();
  private readonly target = zeroes();
  private readonly seen = new Set<FaceChannel>();
  private w = 0;
  private t = 0;
  private last = -Infinity;
  private on = false;

  /** A new sample, or `null` to stop (she eases back). */
  set(d: FaceDrive | null): void {
    if (!d) {
      this.on = false;
      return;
    }
    if (!this.on && this.w < 0.001) {
      // A fresh start: no stale pose from an earlier session.
      this.seen.clear();
      for (const c of FACE_CHANNELS) this.value[c] = this.target[c] = 0;
    }
    this.on = true;
    this.last = this.t;
    for (const c of FACE_CHANNELS) {
      const v = d[c];
      if (typeof v !== 'number' || !Number.isFinite(v)) continue;
      this.target[c] = limit(c, v);
      if (!this.seen.has(c)) {
        this.seen.add(c);
        this.value[c] = this.target[c];
      }
    }
  }

  /** Samples are arriving (set and not timed out). */
  get live(): boolean {
    return this.on && this.t - this.last < this.timeout;
  }

  /** Blend weight over her own motion, 0..1. */
  get weight(): number {
    return this.w;
  }

  /** The smoothed values (for tests and debugging). */
  get current(): Readonly<Record<FaceChannel, number>> {
    return this.value;
  }

  update(dt: number): void {
    dt = clamp(dt, 0, 0.1);
    this.t += dt;
    const goal = this.live ? 1 : 0;
    this.w = mix(this.w, goal, 1 - Math.exp(-dt / this.fade));
    if (goal === 0 && this.w < 0.001) this.w = 0;
    for (const c of FACE_CHANNELS) this.value[c] = mix(this.value[c], this.target[c], 1 - Math.exp(-dt / TAU[c]));
  }

  /** Writes the drive over a procedural frame (in place). */
  apply(f: AnimFrame): void {
    const w = this.w;
    if (w <= 0) return;
    const v = this.value;
    const b = f.bones;
    const has = (c: FaceChannel) => this.seen.has(c);
    if (has('yaw') || has('pitch') || has('roll')) {
      const head = 1 - NECK_SHARE;
      b.head.x = mix(b.head.x, v.pitch * head, w);
      b.head.y = mix(b.head.y, v.yaw * head, w);
      b.head.z = mix(b.head.z, v.roll * head, w);
      b.neck.x = mix(b.neck.x, v.pitch * NECK_SHARE, w);
      b.neck.y = mix(b.neck.y, v.yaw * NECK_SHARE, w);
      b.neck.z = mix(b.neck.z, v.roll * NECK_SHARE, w);
    }
    if (has('eyeYaw') || has('eyePitch')) {
      for (const e of [b.eye_l, b.eye_r, b.eyes]) {
        e.x = mix(e.x, v.eyePitch, w);
        e.y = mix(e.y, v.eyeYaw, w);
      }
    }
    const m = f.morphs;
    if (has('blinkL') || has('blinkR')) {
      // blink moves both eyes, blink_l / blink_r one half each: the common part plus the difference.
      const l = v.blinkL;
      const r = v.blinkR;
      const both = Math.min(l, r);
      m.blink = mix(m.blink, both, w);
      m.blink_l = mix(m.blink_l, l - both, w);
      m.blink_r = mix(m.blink_r, r - both, w);
    }
    // Mouth shapes add to the lip-sync (she can still talk while mirroring you).
    m.mouth_a = Math.max(m.mouth_a, v.jawOpen * w);
    m.mouth_o = Math.max(m.mouth_o, v.mouthFunnel * w);
    m.mouth_u = Math.max(m.mouth_u, v.mouthPucker * w);
    m.mouth_i = Math.max(m.mouth_i, v.mouthWide * 0.8 * w);
    if (has('smile')) m.smile = mix(m.smile, v.smile, w);
    if (has('browUp')) m.surprised = mix(m.surprised, v.browUp * 0.6, w);
    if (has('browDown')) m.angry = mix(m.angry, v.browDown * 0.6, w);
  }

  reset(): void {
    this.on = false;
    this.w = 0;
    this.seen.clear();
    for (const c of FACE_CHANNELS) this.value[c] = this.target[c] = 0;
  }
}
