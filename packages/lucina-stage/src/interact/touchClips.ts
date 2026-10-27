/**
 * Procedural gestures for touch reactions (additive, work with or without the clip file):
 * `pat` (leans into the hand, eyes closed, smiling), `flinch` (startled pull-back), `pout`
 * (turns away a little, cross), `giggle` (shoulders shake with a smile), `look` (glances down at
 * where she was touched).
 */
import { clamp } from '../types';
import { registerClip } from '../scene/motion';

const TAU = Math.PI * 2;
let registered = false;

/** Registers the touch gestures (idempotent). Called by the stage; an explicit call, because the
 * package is marked side-effect free and a bare import would be tree-shaken away. */
export function registerTouchClips(): void {
  if (registered) return;
  registered = true;

function env(t: number, dur: number, fadeIn = 0.15, fadeOut = 0.35): number {
  const a = clamp(t / fadeIn, 0, 1);
  const b = clamp((dur - t) / fadeOut, 0, 1);
  const s = (x: number) => x * x * (3 - 2 * x);
  return s(a) * s(b);
}

registerClip('pat', {
  duration: 1.8,
  sample: (t) => {
    const e = env(t, 1.8, 0.2, 0.5);
    const sway = Math.sin(t * TAU * 0.9) * e;
    return {
      bones: { head: { x: 0.1 * e, z: 0.07 * sway }, neck: { x: 0.04 * e }, chest: { x: 0.015 * e } },
      morphs: { blink: 0.92 * e, smile: 1 * e },
    };
  },
});

registerClip('flinch', {
  duration: 1.1,
  sample: (t) => {
    // Fast in, slow out.
    const e = env(t, 1.1, 0.07, 0.7);
    return {
      bones: { spine: { x: -0.05 * e }, chest: { x: -0.07 * e }, head: { x: -0.08 * e, y: 0.05 * e }, neck: { x: -0.03 * e } },
      morphs: { surprised: 0.8 * e },
    };
  },
});

registerClip('pout', {
  duration: 1.9,
  sample: (t) => {
    const e = env(t, 1.9, 0.25, 0.5);
    return {
      bones: { head: { y: 0.22 * e, x: -0.04 * e, z: -0.05 * e }, neck: { y: 0.08 * e }, chest: { y: 0.04 * e } },
      morphs: { angry: 0.55 * e },
    };
  },
});

registerClip('giggle', {
  duration: 1.5,
  sample: (t, c) => {
    const e = env(t, 1.5, 0.1, 0.4);
    const shake = Math.sin(t * TAU * 4.2) * e;
    return {
      bones: {
        chest: { x: 0.02 * Math.abs(shake) },
        head: { x: 0.05 * e, z: 0.04 * shake },
        upper_arm_l: { z: c.leftSide * 0.03 * Math.abs(shake) },
        upper_arm_r: { z: -c.leftSide * 0.03 * Math.abs(shake) },
      },
      morphs: { smile: 1 * e, blink: 0.5 * e },
    };
  },
});

registerClip('look', {
  duration: 1.5,
  sample: (t) => {
    const e = env(t, 1.5, 0.2, 0.45);
    return { bones: { head: { x: 0.16 * e, z: 0.06 * e }, neck: { x: 0.06 * e } } };
  },
});

// Dance mode without the clip file: a bouncy groove at 120 BPM until stopped (long duration).
registerClip('dance', {
  duration: 3600,
  sample: (t, c) => {
    const e = Math.min(t / 0.4, 1);
    const b = Math.sin(t * TAU * 2);
    const sway = Math.sin(t * TAU);
    return {
      bones: {
        spine: { z: 0.06 * sway * e, x: 0.03 * Math.abs(b) * e },
        chest: { z: -0.05 * sway * e, y: 0.08 * sway * e },
        head: { z: 0.08 * sway * e, x: -0.04 * Math.abs(b) * e },
        upper_arm_l: { z: c.leftSide * (0.35 + 0.15 * b) * e, x: -0.2 * e },
        upper_arm_r: { z: -c.leftSide * (0.35 - 0.15 * b) * e, x: -0.2 * e },
        lower_arm_l: { x: -0.5 * e },
        lower_arm_r: { x: -0.5 * e },
      },
      morphs: { smile: 0.8 * e },
    };
  },
});
}
