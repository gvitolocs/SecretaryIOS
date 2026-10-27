import { describe, expect, it } from 'vitest';
import { findName, normalizeManifest, resolveBones, resolveMorphs, sanitizeNodeName, type MeshMorphInfo } from '../src/manifest';

describe('normalizeManifest', () => {
  it('tolerates garbage', () => {
    for (const raw of [null, undefined, 42, 'x', [], { morphs: 5, bones: 'no' }]) {
      const m = normalizeManifest(raw);
      expect(m.morphs).toEqual({});
      expect(m.bones).toEqual({});
    }
  });

  it('reads the converter format (logical keys, bone_names_for_stage, camera_framing, idle)', () => {
    const m = normalizeManifest({
      forward: '+Z (model faces +Z)',
      morphs: { blink: { index: 0, original_name: 'まばたき' }, mouth_a: { index: 3 }, 'ファルシオンOFF': { index: 71 } },
      bone_names_for_stage: { head: 'head', eye_left: 'leftEye', eye_right: 'rightEye', eyes_both: 'eyes', upper_chest: null },
      bones: { head: { original_name: '頭' } },
      camera_framing: {
        head_and_shoulders: { position: [0, 1.385, 1.078], target: [0, 1.365, 0], fov_y_deg: 30 },
        full_body: { position: [0, 0.9, 3.4], target: [0, 0.8, 0], fov_y_deg: 30 },
      },
      recommended_idle: {
        bone_local_rotations: { leftUpperArm: { world_axis: [0, 0, 1], world_angle_deg: -30 } },
        morphs: { 'ファルシオンOFF': 1 },
      },
    });
    expect(m.facing).toBe('+z');
    expect(m.morphs.blink).toEqual(['blink', 'まばたき']);
    expect(m.morphs.mouth_a).toEqual(['mouth_a']);
    expect(m.bones.head).toEqual(['head', 'head', '頭']);
    expect(m.bones.eye_l).toEqual(['leftEye']);
    expect(m.bones.eyes).toEqual(['eyes']);
    expect(m.camera.bust).toEqual({ position: [0, 1.385, 1.078], target: [0, 1.365, 0], fov: 30 });
    expect(m.camera.full?.fov).toBe(30);
    expect(m.idle.rotations.leftUpperArm).toEqual({ axis: [0, 0, 1], angleDeg: -30 });
    expect(m.idle.morphs).toEqual({ 'ファルシオンOFF': 1 });
  });

  it('accepts plain string maps and an eyes pair', () => {
    const m = normalizeManifest({ morphs: { smile: '笑い', A: 'あ' }, bones: { eyes: { left: 'L_eye', right: 'R_eye' } } });
    expect(m.morphs.smile).toEqual(['笑い']);
    expect(m.morphs.mouth_a).toEqual(['あ']);
    expect(m.bones.eye_l).toEqual(['L_eye']);
    expect(m.bones.eye_r).toEqual(['R_eye']);
  });
});

describe('name matching', () => {
  it('matches exact, sanitised and loose names', () => {
    expect(sanitizeNodeName('Upper Arm.L')).toBe('Upper_ArmL');
    expect(findName('Upper Arm.L', ['Upper_ArmL'])).toBe('Upper_ArmL');
    expect(findName('LEFT_EYE', ['leftEye'])).toBe('leftEye');
    expect(findName('nothing', ['head'])).toBeUndefined();
  });
});

describe('resolveMorphs', () => {
  const meshes: MeshMorphInfo[] = [
    { name: 'Face_1', dictionary: { blink_l: 0, blink_r: 1, あ: 2, 笑い: 3 } },
    { name: 'Face_2', dictionary: { blink_l: 0, blink_r: 1, あ: 2 } },
  ];

  it('binds every mesh that has the morph and falls back to known names', () => {
    const r = resolveMorphs(normalizeManifest(null), meshes);
    expect(r.bindings.mouth_a?.map((b) => b.mesh)).toEqual([0, 1]);
    expect(r.bindings.smile).toEqual([{ mesh: 0, index: 3, name: '笑い' }]);
  });

  it('derives blink from blink_l + blink_r', () => {
    const r = resolveMorphs(normalizeManifest(null), meshes);
    expect(r.bindings.blink?.length).toBe(4);
  });

  it('reports missing morphs instead of failing, and skips wrong manifest names', () => {
    const r = resolveMorphs(normalizeManifest({ morphs: { sad: 'does-not-exist' } }), meshes);
    expect(r.missing).toEqual(expect.arrayContaining(['sad', 'angry', 'surprised', 'mouth_i', 'mouth_o']));
    expect(r.bindings.sad).toBeUndefined();
  });

  it('handles a model without morphs', () => {
    const r = resolveMorphs(normalizeManifest(null), []);
    expect(Object.keys(r.bindings)).toHaveLength(0);
    expect(r.missing).toHaveLength(12);
  });
});

describe('resolveBones', () => {
  it('prefers manifest names, then defaults, and reports missing ones', () => {
    const nodes = ['頭', '首', '上半身', 'leftEye', 'R_eye_custom'];
    const r = resolveBones(normalizeManifest({ bones: { eye_r: 'R_eye_custom' } }), nodes);
    expect(r.bones).toMatchObject({ head: '頭', neck: '首', spine: '上半身', eye_l: 'leftEye', eye_r: 'R_eye_custom' });
    expect(r.missing).toEqual(expect.arrayContaining(['chest', 'eyes', 'upper_arm_l']));
  });

  it('never binds two logical bones to one node', () => {
    const r = resolveBones(normalizeManifest({ bones: { chest: 'spine', neck: 'head' } }), ['spine', 'head']);
    expect(r.bones.spine).toBe('spine');
    expect(r.bones.chest).toBeUndefined();
    expect(r.bones.head).toBe('head');
    expect(r.bones.neck).toBeUndefined();
  });
});

describe('manifest state clip pools', () => {
  it('reads state_clips for known states only, keeping string names', () => {
    const m = normalizeManifest({
      state_clips: { idle: ['status_loop', 'relax_loop', 3, ''], speaking: 'talk_left', dancing: ['x'] },
    });
    expect(m.stateClips).toEqual({ idle: ['status_loop', 'relax_loop'] });
    expect(normalizeManifest({}).stateClips).toEqual({});
  });

  it('reads pose.armSpreadDeg, clamped, 0 when absent or not a number', () => {
    expect(normalizeManifest({ pose: { armSpreadDeg: 8 } }).pose.armSpreadDeg).toBe(8);
    expect(normalizeManifest({ pose: { armSpreadDeg: 90 } }).pose.armSpreadDeg).toBe(25);
    expect(normalizeManifest({ pose: { armSpreadDeg: '8' } }).pose.armSpreadDeg).toBe(0);
    expect(normalizeManifest({}).pose.armSpreadDeg).toBe(0);
  });
});
