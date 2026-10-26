/**
 * Additive body layer on top of the Animator: relaxed arm sway in time with her breath, and
 * short procedural gestures ("clips") such as a nod, a happy bounce or a shy glance.
 *
 * `stage.gesture(name)` runs one from the registry; `registerClip(name, clip)` adds more. These are
 * additive and work with or without the keyframed clip file (`stage.play`, src/clips.ts): with clips
 * the idle sway is left to the clip and only the gestures are layered on top.
 */
import type { AnimFrame } from '../animator';
import { clamp, type LogicalBone, type LogicalMorph, type RotDelta, type StageState } from '../types';

const TAU = Math.PI * 2;

/** What a clip contributes at time `t` (seconds since it started). */
export interface ClipSample {
  bones?: Partial<Record<LogicalBone, Partial<RotDelta>>>;
  morphs?: Partial<Record<LogicalMorph, number>>;
}

export interface Clip {
  duration: number;
  sample(t: number, ctx: ClipContext): ClipSample;
}

export interface ClipContext {
  /** +1 if her left arm is on +X in character space, else -1. */
  leftSide: number;
}

/** Smooth in/out envelope over [0, dur]. */
function env(t: number, dur: number, fadeIn = 0.18, fadeOut = 0.3): number {
  const a = clamp(t / fadeIn, 0, 1);
  const b = clamp((dur - t) / fadeOut, 0, 1);
  const s = (x: number) => x * x * (3 - 2 * x);
  return s(a) * s(b);
}

const registry = new Map<string, Clip>();

export function registerClip(name: string, clip: Clip): void {
  registry.set(name, clip);
}

export function clipNames(): string[] {
  return [...registry.keys()];
}

// ---------- Built-in procedural clips ----------

registerClip('nod', {
  duration: 1.1,
  sample: (t) => {
    const e = env(t, 1.1, 0.08, 0.25);
    const n = Math.sin(t * TAU * 1.8) * 0.5 + 0.5;
    return { bones: { head: { x: 0.13 * n * e }, neck: { x: 0.05 * n * e } }, morphs: { smile: 0.35 * e } };
  },
});

registerClip('tilt', {
  duration: 1.8,
  sample: (t) => {
    const e = env(t, 1.8, 0.3, 0.5);
    return { bones: { head: { z: 0.16 * e, x: 0.03 * e }, neck: { z: 0.05 * e } }, morphs: { smile: 0.55 * e } };
  },
});

registerClip('happy', {
  duration: 1.6,
  sample: (t, c) => {
    const e = env(t, 1.6, 0.12, 0.45);
    const bounce = Math.abs(Math.sin(t * TAU * 1.4)) * e;
    return {
      bones: {
        spine: { x: -0.025 * bounce },
        chest: { x: -0.035 * bounce },
        head: { x: -0.05 * e, z: 0.06 * Math.sin(t * TAU * 0.7) * e },
        upper_arm_l: { z: c.leftSide * 0.07 * bounce },
        upper_arm_r: { z: -c.leftSide * 0.07 * bounce },
      },
      morphs: { smile: 0.95 * e },
    };
  },
});

registerClip('shy', {
  duration: 2.2,
  sample: (t) => {
    const e = env(t, 2.2, 0.35, 0.6);
    return {
      bones: { head: { x: 0.12 * e, y: 0.18 * e, z: -0.06 * e }, neck: { x: 0.04 * e, y: 0.06 * e }, chest: { y: 0.04 * e } },
      morphs: { smile: 0.45 * e, blink: 0.25 * e },
    };
  },
});

registerClip('surprised', {
  duration: 1.4,
  sample: (t, c) => {
    const e = env(t, 1.4, 0.06, 0.5);
    return {
      bones: {
        head: { x: -0.08 * e },
        chest: { x: -0.03 * e },
        upper_arm_l: { z: c.leftSide * 0.08 * e, x: -0.06 * e },
        upper_arm_r: { z: -c.leftSide * 0.08 * e, x: -0.06 * e },
      },
      morphs: { surprised: 0.8 * e },
    };
  },
});

registerClip('greet', {
  duration: 2,
  sample: (t) => {
    const e = env(t, 2, 0.25, 0.5);
    const n = t < 1 ? Math.sin((t / 1) * Math.PI) : 0;
    return { bones: { head: { x: 0.1 * n, z: 0.05 * e }, spine: { x: 0.03 * n }, chest: { x: 0.04 * n } }, morphs: { smile: 0.8 * e } };
  },
});

registerClip('think', {
  duration: 2.6,
  sample: (t) => {
    const e = env(t, 2.6, 0.4, 0.6);
    return { bones: { head: { z: -0.09 * e, x: -0.05 * e, y: -0.1 * e }, eyes: { x: -0.12 * e, y: -0.15 * e }, eye_l: { x: -0.12 * e, y: -0.15 * e }, eye_r: { x: -0.12 * e, y: -0.15 * e } } };
  },
});

/** Reactions to a tap on her, in rotation so she never repeats the same one twice in a row. */
export const TAP_REACTIONS = ['happy', 'tilt', 'nod', 'shy', 'surprised', 'greet'] as const;

export class MotionLayer {
  private t = 0;
  private active: { clip: Clip; name: string; t: number } | null = null;
  private lastReaction = '';
  private readonly ctx: ClipContext;

  constructor(
    leftSide: number,
    private readonly random: () => number = Math.random,
  ) {
    this.ctx = { leftSide: leftSide >= 0 ? 1 : -1 };
  }

  get playing(): string | null {
    return this.active?.name ?? null;
  }

  play(name: string): boolean {
    const clip = registry.get(name);
    if (!clip) return false;
    this.active = { clip, name, t: 0 };
    return true;
  }

  /** Picks and plays a procedural tap reaction; returns its name. */
  react(): string {
    const options = TAP_REACTIONS.filter((n) => n !== this.lastReaction && registry.has(n));
    const name = options[Math.floor(this.random() * options.length)] ?? 'nod';
    this.lastReaction = name;
    this.play(name);
    return name;
  }

  stop(): void {
    this.active = null;
  }

  /** Adds the layer to `frame` in place. */
  apply(frame: AnimFrame, dt: number, state: StageState, reduced: boolean, clipDriven = false): AnimFrame {
    this.t += dt;
    if (reduced) {
      this.active = null;
      return frame;
    }
    const t = this.t;
    const b = frame.bones;
    const L = this.ctx.leftSide;
    if (!clipDriven) this.idleSway(b, t, L, state);
    this.applyClip(frame, dt);
    return frame;
  }

  private idleSway(b: AnimFrame['bones'], t: number, L: number, state: StageState): void {
    // Arms hang and swing a little with the breath (Animator breath: 0.25 Hz), never perfectly still.
    const breath = Math.sin(TAU * 0.25 * t);
    const swayL = 0.018 * Math.sin(TAU * 0.11 * t + 0.4) + 0.008 * breath;
    const swayR = 0.018 * Math.sin(TAU * 0.11 * t + 2.1) + 0.008 * breath;
    const energy = state === 'speaking' ? 1.6 : state === 'listening' ? 0.6 : 1;
    b.upper_arm_l.z += L * (0.012 + 0.01 * breath) * energy;
    b.upper_arm_r.z += -L * (0.012 + 0.01 * breath) * energy;
    b.upper_arm_l.x += swayL * energy;
    b.upper_arm_r.x += swayR * energy;
    b.lower_arm_l.x += -0.02 * (0.5 + 0.5 * Math.sin(TAU * 0.09 * t + 1)) * energy;
    b.lower_arm_r.x += -0.02 * (0.5 + 0.5 * Math.sin(TAU * 0.09 * t + 2.5)) * energy;
    // Slow weight shift through the torso, a bit wider than the Animator's.
    b.spine.z += 0.01 * Math.sin(TAU * 0.037 * t + 0.8);
    b.chest.z += -0.006 * Math.sin(TAU * 0.037 * t + 0.8);
    b.chest.y += 0.008 * Math.sin(TAU * 0.029 * t);
  }

  private applyClip(frame: AnimFrame, dt: number): void {
    const b = frame.bones;
    const a = this.active;
    if (a) {
      a.t += dt;
      if (a.t >= a.clip.duration) this.active = null;
      else {
        const s = a.clip.sample(a.t, this.ctx);
        if (s.bones) {
          for (const [k, d] of Object.entries(s.bones) as [LogicalBone, Partial<RotDelta>][]) {
            const r = b[k];
            if (!r || !d) continue;
            r.x += d.x ?? 0;
            r.y += d.y ?? 0;
            r.z += d.z ?? 0;
          }
        }
        if (s.morphs) {
          for (const [k, w] of Object.entries(s.morphs) as [LogicalMorph, number][]) {
            frame.morphs[k] = Math.max(frame.morphs[k] ?? 0, w);
          }
        }
      }
    }
  }
}
