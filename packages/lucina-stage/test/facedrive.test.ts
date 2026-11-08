import { Euler, Matrix4, Quaternion, Vector3 } from 'three';
import { describe, expect, it } from 'vitest';
import type { AnimFrame } from '../src/animator';
import { FaceDriver, FaceSolver, HeadPoseSolver, faceDriveFromBlendshapes, ramp, NECK_SHARE } from '../src/facedrive';
import { LOGICAL_BONES, LOGICAL_MORPHS, type LogicalBone, type LogicalMorph, type RotDelta } from '../src/types';

function frame(): AnimFrame {
  const bones = {} as Record<LogicalBone, RotDelta>;
  for (const b of LOGICAL_BONES) bones[b] = { x: 0, y: 0, z: 0 };
  const morphs = {} as Record<LogicalMorph, number>;
  for (const m of LOGICAL_MORPHS) morphs[m] = 0;
  return { bones, morphs };
}

/** Face transform (column-major) for an anatomical yaw/pitch/roll, on top of a fixed camera offset. */
function faceMatrix(yaw: number, pitch: number, roll: number, base = new Quaternion()): number[] {
  const q = base.clone().multiply(new Quaternion().setFromEuler(new Euler(pitch, yaw, roll, 'YXZ')));
  return new Matrix4().compose(new Vector3(0.02, -0.05, -0.35), q, new Vector3(1, 1, 1)).toArray();
}

function run(d: FaceDriver, seconds: number, step = 1 / 60): void {
  for (let t = 0; t < seconds; t += step) d.update(step);
}

describe('faceDriveFromBlendshapes', () => {
  it('ramps coefficients and mirrors sides by default', () => {
    const d = faceDriveFromBlendshapes({ eyeBlinkLeft: 0.9, eyeBlinkRight: 0.05, jawOpen: 0.5, mouthSmileLeft: 0.7, mouthSmileRight: 0.7 });
    // The user's left eye closes: in a mirror that is her right eye.
    expect(d.blinkR).toBe(1);
    expect(d.blinkL).toBe(0);
    expect(d.jawOpen).toBe(1);
    expect(d.smile).toBe(1);
    expect(d.yaw).toBeUndefined();
  });

  it('puppet mode keeps sides; swapSides reads the other coefficient', () => {
    const s = { eyeBlinkLeft: 0.9 };
    expect(faceDriveFromBlendshapes(s, null, { mirror: false }).blinkL).toBe(1);
    expect(faceDriveFromBlendshapes(s, null, { mirror: false, swapSides: true }).blinkR).toBe(1);
  });

  it('mirrors yaw and roll, not pitch', () => {
    const d = faceDriveFromBlendshapes({}, { yaw: 0.3, pitch: 0.2, roll: 0.1 });
    expect(d.yaw).toBeCloseTo(-0.3);
    expect(d.pitch).toBeCloseTo(0.2);
    expect(d.roll).toBeCloseTo(-0.1);
    const p = faceDriveFromBlendshapes({}, { yaw: 0.3, pitch: 0.2, roll: 0.1 }, { mirror: false });
    expect(p.yaw).toBeCloseTo(0.3);
    expect(p.roll).toBeCloseTo(0.1);
  });

  it('gaze toward the user’s left turns her eyes to the screen’s left when mirrored', () => {
    const d = faceDriveFromBlendshapes({ eyeLookOutLeft: 1, eyeLookInRight: 1 });
    expect(d.eyeYaw!).toBeLessThan(0);
    expect(faceDriveFromBlendshapes({ eyeLookDownLeft: 1, eyeLookDownRight: 1 }).eyePitch!).toBeGreaterThan(0);
  });

  it('ignores junk', () => {
    expect(ramp(Number.NaN, 0, 1)).toBe(0);
    expect(faceDriveFromBlendshapes({ jawOpen: Number.NaN }).jawOpen).toBe(0);
  });
});

describe('HeadPoseSolver', () => {
  it('calibrates on the first frames, then reports the delta in face axes', () => {
    // The phone held below and to the side: the neutral face is already rotated in camera space.
    const base = new Quaternion().setFromEuler(new Euler(-0.35, 0.2, 0.05, 'YXZ'));
    const s = new HeadPoseSolver();
    for (let i = 0; i < s.calibrationFrames - 1; i++) expect(s.push(faceMatrix(0, 0, 0, base))).toBeNull();
    expect(s.push(faceMatrix(0, 0, 0, base))).toEqual({ yaw: expect.closeTo(0, 5), pitch: expect.closeTo(0, 5), roll: expect.closeTo(0, 5) });
    const p = s.push(faceMatrix(0.4, -0.2, 0.15, base))!;
    expect(p.yaw).toBeCloseTo(0.4, 4);
    expect(p.pitch).toBeCloseTo(-0.2, 4);
    expect(p.roll).toBeCloseTo(0.15, 4);
    s.recenter();
    expect(s.calibrated).toBe(false);
  });

  it('rejects malformed matrices', () => {
    const s = new HeadPoseSolver();
    expect(s.push([1, 2, 3])).toBeNull();
    expect(s.push(new Array(16).fill(Number.NaN))).toBeNull();
  });

  it('FaceSolver combines both', () => {
    const solver = new FaceSolver();
    let d = solver.solve({ jawOpen: 0.3 }, faceMatrix(0, 0, 0));
    for (let i = 1; i < solver.head.calibrationFrames; i++) d = solver.solve({ jawOpen: 0.3 }, faceMatrix(0, 0, 0));
    expect(solver.head.calibrated).toBe(true);
    d = solver.solve({ jawOpen: 0.3 }, faceMatrix(0.25, 0, 0));
    expect(d.yaw).toBeCloseTo(-0.25, 3); // mirrored
    expect(d.jawOpen).toBeGreaterThan(0.4);
  });
});

describe('FaceDriver', () => {
  it('eases in, drives head/neck/eyes/morphs, and eases out when samples stop', () => {
    const d = new FaceDriver();
    const sample = { yaw: 0.5, pitch: -0.2, roll: 0.1, blinkL: 1, blinkR: 0.4, jawOpen: 0.8, smile: 0.6, eyeYaw: 0.1 };
    d.set(sample);
    for (let i = 0; i < 30; i++) {
      d.set(sample);
      d.update(1 / 60);
    }
    expect(d.live).toBe(true);
    expect(d.weight).toBeGreaterThan(0.85);
    const f = frame();
    f.morphs.mouth_a = 0.2;
    f.morphs.blink = 1; // a procedural blink is overridden
    d.apply(f);
    const w = d.weight;
    expect(f.bones.head.y).toBeCloseTo(0.5 * (1 - NECK_SHARE) * w, 5);
    expect(f.bones.neck.y).toBeCloseTo(0.5 * NECK_SHARE * w, 5);
    expect(f.bones.eye_l.y).toBeCloseTo(0.1 * w, 5);
    expect(f.morphs.mouth_a).toBeCloseTo(0.8 * w, 5);
    // Both eyes share 0.4, the left gets the rest.
    expect(f.morphs.blink).toBeCloseTo(1 + (0.4 - 1) * w, 5);
    expect(f.morphs.blink_l).toBeCloseTo(0.6 * w, 5);
    expect(f.morphs.blink_r).toBeCloseTo(0, 5);
    // No samples: lost after the timeout, then fades out.
    run(d, 0.6);
    expect(d.live).toBe(false);
    run(d, 2);
    expect(d.weight).toBe(0);
    const g = frame();
    d.apply(g);
    expect(g.bones.head.y).toBe(0);
  });

  it('clamps rotations and weights', () => {
    const d = new FaceDriver();
    d.set({ yaw: 9, jawOpen: 4, blinkL: -1 });
    expect(d.current.yaw).toBeLessThan(1);
    expect(d.current.jawOpen).toBe(1);
    expect(d.current.blinkL).toBe(0);
  });

  it('null stops it; a later start begins from a clean pose', () => {
    const d = new FaceDriver();
    d.set({ yaw: 0.6 });
    run(d, 0.3);
    d.set(null);
    expect(d.live).toBe(false);
    run(d, 3);
    d.set({ jawOpen: 0.5 });
    expect(d.current.yaw).toBe(0);
    const f = frame();
    d.update(1 / 60);
    d.apply(f);
    expect(f.bones.head.y).toBe(0); // no head channel in this session
  });
});
