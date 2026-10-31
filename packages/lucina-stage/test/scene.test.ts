import { describe, expect, it } from 'vitest';
import { Animator } from '../src/animator';
import { clipNames, MotionLayer, registerClip, TAP_REACTIONS } from '../src/scene/motion';
import { detectQuality, isStageQuality, lowerQuality, QUALITY_PROFILES } from '../src/scene/quality';
import { zeroVisemes } from '../src/types';

const frame = () => new Animator({ random: () => 0.5 }).update(0.016, 'idle', zeroVisemes());

describe('quality', () => {
  it('picks a tier from the device', () => {
    expect(detectQuality({ coarsePointer: false, shortSide: 1080, cores: 8, memoryGb: 8 })).toBe('high');
    expect(detectQuality({ coarsePointer: true, shortSide: 390, cores: 6, memoryGb: 4 })).toBe('medium');
    expect(detectQuality({ coarsePointer: true, shortSide: 390, cores: 4, memoryGb: 4 })).toBe('low');
    expect(detectQuality({ cores: 2 })).toBe('low');
  });
  it('steps down and validates settings', () => {
    expect(lowerQuality('high')).toBe('medium');
    expect(lowerQuality('low')).toBeNull();
    expect(isStageQuality('auto')).toBe(true);
    expect(isStageQuality('ultra')).toBe(false);
    expect(QUALITY_PROFILES.low.dpr).toBeLessThan(QUALITY_PROFILES.high.dpr);
    expect(QUALITY_PROFILES.high.bloom).toBe(true);
  });
});

describe('MotionLayer', () => {
  it('sways the arms when no clip drives them, and not in reduced motion', () => {
    const m = new MotionLayer(1);
    const f = m.apply(frame(), 1.3, 'idle', false);
    expect(Math.abs(f.bones.upper_arm_l.z) + Math.abs(f.bones.upper_arm_l.x)).toBeGreaterThan(0);
    const still = new MotionLayer(1).apply(frame(), 1.3, 'idle', true);
    expect(still.bones.upper_arm_l).toEqual({ x: 0, y: 0, z: 0 });
    const driven = new MotionLayer(1).apply(frame(), 1.3, 'idle', false, true);
    expect(driven.bones.upper_arm_l).toEqual({ x: 0, y: 0, z: 0 });
  });

  it('plays gestures by name and ends them', () => {
    const m = new MotionLayer(1);
    expect(m.play('nope')).toBe(false);
    expect(m.play('tilt')).toBe(true);
    const f = m.apply(frame(), 0.6, 'idle', false, true);
    expect(f.bones.head.z).toBeGreaterThan(0.05);
    expect(f.morphs.smile).toBeGreaterThan(0.2);
    m.apply(frame(), 2, 'idle', false, true);
    expect(m.playing).toBeNull();
  });

  it('never repeats a tap reaction twice in a row', () => {
    let r = 0;
    const m = new MotionLayer(1, () => (r = (r + 0.37) % 1));
    let last = '';
    for (let i = 0; i < 12; i++) {
      const name = m.react();
      expect(TAP_REACTIONS).toContain(name);
      expect(name).not.toBe(last);
      last = name;
    }
  });

  it('accepts registered clips', () => {
    registerClip('test_wiggle', { duration: 1, sample: () => ({ bones: { chest: { y: 0.2 } } }) });
    expect(clipNames()).toContain('test_wiggle');
    const m = new MotionLayer(1);
    m.play('test_wiggle');
    expect(m.apply(frame(), 0.1, 'idle', false, true).bones.chest.y).toBeCloseTo(0.2, 5);
  });
});
