import { describe, expect, it } from 'vitest';
import { AnimationClip, Bone, Group, Quaternion, QuaternionKeyframeTrack, Vector3 } from 'three';
import { ClipPlayer, clipLibraryFrom, type ClipInfo } from '../src/clips';

function rig(): { root: Group; spine: Bone; arm: Bone } {
  const root = new Group();
  const spine = new Bone();
  spine.name = 'spine';
  const arm = new Bone();
  arm.name = 'leftUpperArm';
  spine.add(arm);
  root.add(spine);
  return { root, spine, arm };
}

function q(axis: [number, number, number], deg: number): number[] {
  return new Quaternion().setFromAxisAngle(new Vector3(...axis), (deg * Math.PI) / 180).toArray();
}

function clip(name: string, duration: number, deg: number, extraNode?: string): AnimationClip {
  const tracks = [
    new QuaternionKeyframeTrack('leftUpperArm.quaternion', [0, duration], [...q([0, 0, 1], deg), ...q([0, 0, 1], deg)]),
  ];
  if (extraNode) tracks.push(new QuaternionKeyframeTrack(`${extraNode}.quaternion`, [0, duration], [0, 0, 0, 1, 0, 0, 0, 1]));
  return new AnimationClip(name, duration, tracks);
}

const meta: ClipInfo[] = [
  { name: 'idle_breathe', duration: 4, loop: true, tags: ['idle'] },
  { name: 'idle_shift', duration: 4, loop: true, tags: ['idle'] },
  { name: 'greet_wave', duration: 1, loop: false, tags: ['greeting'] },
  { name: 'talk_gesture_1', duration: 2, loop: true, tags: ['talk'] },
];

function angleDeg(b: Bone): number {
  return (2 * Math.acos(Math.min(1, Math.abs(b.quaternion.w))) * 180) / Math.PI;
}

describe('ClipPlayer', () => {
  it('starts an idle at once, so the rest pose never shows', () => {
    const { root, arm } = rig();
    const lib = clipLibraryFrom([clip('idle_breathe', 4, -60), clip('idle_shift', 4, -60), clip('greet_wave', 1, 90)], meta);
    const p = new ClipPlayer(root, lib, { random: () => 0 });
    p.update(0);
    expect(p.current).toBe('idle_breathe');
    expect(angleDeg(arm)).toBeCloseTo(60, 0);
  });

  it('plays a one-shot, resolves at its end and crossfades back to idle without a rest-pose frame', async () => {
    const { root, arm } = rig();
    const lib = clipLibraryFrom([clip('idle_breathe', 4, -60), clip('greet_wave', 1, -120)], meta);
    const p = new ClipPlayer(root, lib, { random: () => 0 });
    p.update(0);
    let done = false;
    const pr = p.play('greet_wave', { fade: 0.2 }).then(() => (done = true));
    let minAngle = 999;
    let maxAngle = 0;
    for (let i = 0; i < 120; i++) {
      p.update(1 / 60);
      minAngle = Math.min(minAngle, angleDeg(arm));
      maxAngle = Math.max(maxAngle, angleDeg(arm));
    }
    await pr;
    expect(done).toBe(true);
    expect(p.current).toBe('idle_breathe');
    // the wave was reached, and weights always sum to one (a dip would pull the arm toward rest)
    expect(maxAngle).toBeGreaterThan(115);
    expect(minAngle).toBeGreaterThan(59);
  });

  it('follows the stage state with the default pools', () => {
    const { root } = rig();
    const lib = clipLibraryFrom([clip('idle_breathe', 4, -60), clip('talk_gesture_1', 2, -50)], meta);
    const p = new ClipPlayer(root, lib, { random: () => 0 });
    p.setState('speaking');
    expect(p.current).toBe('talk_gesture_1');
    p.setState('idle');
    expect(p.current).toBe('idle_breathe');
    // a state without clips falls back to the idle pool
    p.setState('thinking');
    expect(p.current).toBe('idle_breathe');
  });

  it('skips tracks for bones the model lacks and rejects unknown clips', async () => {
    const { root } = rig();
    const lib = clipLibraryFrom([clip('idle_breathe', 4, -60, 'leftUpperArmTwist')], meta);
    const p = new ClipPlayer(root, lib, { random: () => 0 });
    expect([...p.drivenNodes]).toEqual(['leftUpperArm']);
    await expect(p.play('nope')).rejects.toThrow(/Unknown clip/);
  });
});
