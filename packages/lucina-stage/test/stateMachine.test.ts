import { describe, expect, it, vi } from 'vitest';
import { Animator } from '../src/animator';
import { PoseBlender, STATE_POSES, StageStateMachine } from '../src/stateMachine';
import { zeroVisemes, type StageState } from '../src/types';

describe('StageStateMachine', () => {
  it('starts idle and notifies on changes only', () => {
    const m = new StageStateMachine();
    const cb = vi.fn();
    m.onChange(cb);
    expect(m.state).toBe('idle');
    expect(m.set('idle')).toBe(false);
    expect(m.set('listening')).toBe(true);
    expect(cb).toHaveBeenCalledWith('listening', 'idle');
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('rejects unknown states', () => {
    const m = new StageStateMachine();
    expect(() => m.set('dancing' as StageState)).toThrow(TypeError);
    expect(m.state).toBe('idle');
  });

  it('returns to the previous state after speaking', () => {
    const m = new StageStateMachine();
    m.set('listening');
    m.beginSpeaking();
    expect(m.state).toBe('speaking');
    expect(m.endSpeaking()).toBe('listening');
  });

  it('never returns to thinking after speaking', () => {
    const m = new StageStateMachine();
    m.set('thinking');
    m.beginSpeaking();
    expect(m.endSpeaking()).toBe('idle');
  });

  it('keeps a state the app set while audio was playing', () => {
    const m = new StageStateMachine();
    m.beginSpeaking();
    m.set('listening');
    expect(m.endSpeaking()).toBe('listening');
  });

  it('unsubscribes listeners', () => {
    const m = new StageStateMachine();
    const cb = vi.fn();
    const off = m.onChange(cb);
    off();
    m.set('thinking');
    expect(cb).not.toHaveBeenCalled();
  });
});

describe('PoseBlender', () => {
  it('blends smoothly toward the state pose', () => {
    const b = new PoseBlender('idle');
    const first = b.update('thinking', 1 / 60).gazeYaw;
    expect(first).not.toBe(STATE_POSES.thinking.gazeYaw);
    for (let i = 0; i < 240; i++) b.update('thinking', 1 / 60);
    expect(b.pose.gazeYaw).toBeCloseTo(STATE_POSES.thinking.gazeYaw, 3);
    expect(b.pose.blinkSlow).toBeCloseTo(STATE_POSES.thinking.blinkSlow, 3);
  });
});

describe('Animator', () => {
  const rng = (): (() => number) => {
    let s = 42;
    return () => ((s = (s * 1664525 + 1013904223) >>> 0) / 4294967296);
  };

  it('blinks every 2.5-6 s while idle', () => {
    const a = new Animator({ random: rng() });
    let blinks = 0;
    let closed = false;
    for (let i = 0; i < 60 * 60; i++) {
      const b = a.update(1 / 60, 'idle', zeroVisemes()).morphs.blink;
      if (b > 0.9 && !closed) blinks++;
      closed = b > 0.9;
    }
    expect(blinks).toBeGreaterThanOrEqual(60 / 6);
    expect(blinks).toBeLessThanOrEqual((60 / 2.5) * 1.3);
  });

  it('leans in while listening and looks up/aside while thinking', () => {
    const a = new Animator({ random: rng() });
    let f = a.update(0, 'idle', zeroVisemes());
    for (let i = 0; i < 120; i++) f = a.update(1 / 60, 'listening', zeroVisemes());
    expect(f.bones.spine.x).toBeGreaterThan(0.015);
    for (let i = 0; i < 180; i++) f = a.update(1 / 60, 'thinking', zeroVisemes());
    expect(f.bones.eye_l.x).toBeLessThan(-0.05); // up
    expect(Math.abs(f.bones.eye_l.y)).toBeGreaterThan(0.05); // aside
  });

  it('clamps look-at angles', () => {
    const a = new Animator({ random: rng() });
    a.setPointer({ yaw: 5, pitch: -5 });
    let f = a.update(0, 'idle', zeroVisemes());
    for (let i = 0; i < 120; i++) f = a.update(1 / 60, 'idle', zeroVisemes());
    expect(Math.abs(f.bones.head.y + f.bones.neck.y)).toBeLessThan(0.5);
    expect(Math.abs(f.bones.eye_l.y)).toBeLessThanOrEqual(0.33);
  });

  it('reduced motion keeps only blinks, mouth and expressions', () => {
    const a = new Animator({ random: rng(), reducedMotion: true });
    a.setPointer({ yaw: 0.5, pitch: 0.2 });
    a.setExpression('smile', 1);
    let blinked = false;
    let f = a.update(0, 'listening', zeroVisemes());
    for (let i = 0; i < 600; i++) {
      f = a.update(1 / 60, 'listening', { ...zeroVisemes(), a: 0.5 });
      for (const b of Object.values(f.bones)) expect(b).toEqual({ x: 0, y: 0, z: 0 });
      if (f.morphs.blink > 0.5) blinked = true;
    }
    expect(blinked).toBe(true);
    expect(f.morphs.mouth_a).toBe(0.5);
    expect(f.morphs.smile).toBeGreaterThan(0.5);
  });

  it('rejects unknown expressions', () => {
    const a = new Animator();
    expect(() => a.setExpression('wink' as never)).toThrow(TypeError);
  });
});
