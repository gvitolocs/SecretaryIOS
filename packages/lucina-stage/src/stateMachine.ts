/**
 * Conversation state machine + per-state pose targets that the animator blends between.
 */
import { STAGE_STATES, type StageState } from './types';
import { damp } from './spring';

export interface StatePose {
  /** Forward lean of spine/chest in radians (positive toward the viewer). */
  lean: number;
  /** Gaze offset relative to the user, radians (yaw positive = screen right, pitch positive = down). */
  gazeYaw: number;
  gazePitch: number;
  /** Head roll (tilt) in radians. */
  headTilt: number;
  /** Extra head pitch (positive = chin down). */
  headPitch: number;
  /** Multiplier for idle head drift. */
  drift: number;
  /** Blink interval range in seconds, and blink duration multiplier (>1 = slower). */
  blinkMin: number;
  blinkMax: number;
  blinkSlow: number;
  /** Chance per minute-ish of a soft idle smile (0 disables). */
  smileChance: number;
}

export const STATE_POSES: Record<StageState, StatePose> = {
  idle: {
    lean: 0,
    gazeYaw: 0,
    gazePitch: 0,
    headTilt: 0,
    headPitch: 0,
    drift: 1,
    blinkMin: 2.5,
    blinkMax: 6,
    blinkSlow: 1,
    smileChance: 1,
  },
  listening: {
    lean: 0.045,
    gazeYaw: 0,
    gazePitch: 0,
    headTilt: -0.04,
    headPitch: 0.03,
    drift: 0.45,
    blinkMin: 2.8,
    blinkMax: 6,
    blinkSlow: 1,
    smileChance: 0.3,
  },
  thinking: {
    lean: -0.01,
    gazeYaw: -0.32,
    gazePitch: -0.26,
    headTilt: 0.09,
    headPitch: -0.06,
    drift: 0.35,
    blinkMin: 3.5,
    blinkMax: 7,
    blinkSlow: 2.2,
    smileChance: 0,
  },
  speaking: {
    lean: 0.015,
    gazeYaw: 0,
    gazePitch: 0,
    headTilt: 0,
    headPitch: 0,
    drift: 0.7,
    blinkMin: 2.5,
    blinkMax: 5.5,
    blinkSlow: 1,
    smileChance: 0,
  },
};

export type StateListener = (next: StageState, prev: StageState) => void;

export function isStageState(s: unknown): s is StageState {
  return typeof s === 'string' && (STAGE_STATES as readonly string[]).includes(s);
}

export class StageStateMachine {
  private current: StageState;
  private returnState: StageState = 'idle';
  private listeners = new Set<StateListener>();

  constructor(initial: StageState = 'idle') {
    this.current = initial;
  }

  get state(): StageState {
    return this.current;
  }

  /** Changes state; returns false when unchanged. Throws on an unknown state. */
  set(next: StageState): boolean {
    if (!isStageState(next)) throw new TypeError(`Unknown stage state: ${String(next)}`);
    if (next === this.current) return false;
    const prev = this.current;
    this.current = next;
    for (const l of this.listeners) l(next, prev);
    return true;
  }

  /** Enters `speaking`, remembering where to return to afterwards. */
  beginSpeaking(): void {
    if (this.current !== 'speaking') {
      // After a reply the user usually talks next (listening) or nothing happens (idle);
      // "thinking" is never a sensible place to return to.
      this.returnState = this.current === 'thinking' ? 'idle' : this.current;
    }
    this.set('speaking');
  }

  /**
   * Leaves `speaking` for the remembered state. If the app already moved to another state
   * while audio was playing, that state is kept.
   */
  endSpeaking(): StageState {
    if (this.current === 'speaking') this.set(this.returnState);
    return this.current;
  }

  onChange(cb: StateListener): () => void {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }
}

type NumericPoseKey = { [K in keyof StatePose]: StatePose[K] extends number ? K : never }[keyof StatePose];
const POSE_KEYS = Object.keys(STATE_POSES.idle) as NumericPoseKey[];

/** Smoothly blends the active pose toward the current state's target pose. */
export class PoseBlender {
  readonly pose: StatePose;

  constructor(initial: StageState = 'idle') {
    this.pose = { ...STATE_POSES[initial] };
  }

  update(state: StageState, dt: number, tau = 0.35): StatePose {
    const target = STATE_POSES[state];
    for (const k of POSE_KEYS) this.pose[k] = damp(this.pose[k], target[k], tau, dt);
    return this.pose;
  }
}
