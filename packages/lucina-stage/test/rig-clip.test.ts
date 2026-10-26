import { describe, expect, it } from 'vitest';
import { AnimationClip, AnimationMixer, Bone, Group, Quaternion, QuaternionKeyframeTrack, Vector3 } from 'three';
import { normalizeManifest } from '../src/manifest';
import { Rig } from '../src/rig';
import type { AnimFrame } from '../src/animator';
import { LOGICAL_BONES, LOGICAL_MORPHS } from '../src/types';

function still(): AnimFrame {
  return {
    bones: Object.fromEntries(LOGICAL_BONES.map((b) => [b, { x: 0, y: 0, z: 0 }])) as AnimFrame['bones'],
    morphs: Object.fromEntries(LOGICAL_MORPHS.map((m) => [m, 0])) as AnimFrame['morphs'],
  };
}

describe('rig under a clip mixer', () => {
  it('keeps a bone held by a constant clip track, not the rest pose, after the first frame', () => {
    const root = new Group();
    const upper = new Bone();
    upper.name = 'leftUpperArm';
    const lower = new Bone();
    lower.name = 'leftLowerArm';
    upper.add(lower);
    root.add(upper);
    const bent = new Quaternion().setFromAxisAngle(new Vector3(0, 0, 1), 1.2);
    // two equal keys: three's PropertyMixer writes this value once and then skips it as unchanged
    const track = new QuaternionKeyframeTrack('leftLowerArm.quaternion', [0, 3], [...bent.toArray(), ...bent.toArray()]);
    const mixer = new AnimationMixer(root);
    mixer.clipAction(new AnimationClip('hold', 3, [track])).play();
    const r = new Rig(root, normalizeManifest({}));
    const driven = new Set(['leftLowerArm']);
    for (let i = 0; i < 4; i++) {
      r.restoreBones();
      mixer.update(1 / 30);
      r.captureClipPose(driven);
      r.apply(still(), { driven });
      expect(lower.quaternion.angleTo(bent)).toBeLessThan(1e-4);
    }
    r.reset();
    expect(lower.quaternion.angleTo(new Quaternion())).toBeLessThan(1e-6);
  });
});
