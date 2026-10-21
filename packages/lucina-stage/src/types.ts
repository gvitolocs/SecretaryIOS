/** Shared public types of the Lucina stage. */

export type StageState = 'idle' | 'listening' | 'thinking' | 'speaking';
export const STAGE_STATES: readonly StageState[] = ['idle', 'listening', 'thinking', 'speaking'];

export type Expression = 'smile' | 'sad' | 'surprised' | 'angry';
export const EXPRESSIONS: readonly Expression[] = ['smile', 'sad', 'surprised', 'angry'];

export type Framing = 'bust' | 'full';

export const VISEMES = ['a', 'i', 'u', 'e', 'o'] as const;
export type Viseme = (typeof VISEMES)[number];
export type VisemeWeights = Record<Viseme, number>;

/** Logical morph channels the animator drives. The manifest maps them to model morph names. */
export const LOGICAL_MORPHS = [
  'blink',
  'blink_l',
  'blink_r',
  'mouth_a',
  'mouth_i',
  'mouth_u',
  'mouth_e',
  'mouth_o',
  'smile',
  'sad',
  'angry',
  'surprised',
] as const;
export type LogicalMorph = (typeof LOGICAL_MORPHS)[number];

/** Logical bones the animator drives. */
export const LOGICAL_BONES = [
  'spine',
  'chest',
  'neck',
  'head',
  'eye_l',
  'eye_r',
  'eyes',
  'upper_arm_l',
  'upper_arm_r',
  'lower_arm_l',
  'lower_arm_r',
] as const;
export type LogicalBone = (typeof LOGICAL_BONES)[number];

/**
 * A rotation delta in character space (the character faces +Z, Y up):
 * x = pitch (positive leans/looks down, toward the camera),
 * y = yaw (positive turns toward +X, screen right for the viewer),
 * z = roll (positive tilts the top toward -X).
 */
export interface RotDelta {
  x: number;
  y: number;
  z: number;
}

export type Vec3 = [number, number, number];

export function zeroVisemes(): VisemeWeights {
  return { a: 0, i: 0, u: 0, e: 0, o: 0 };
}

export function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}
