/**
 * What she does when you touch her (our own design; Custom Cast has no body-touch reactions, see
 * docs/design/custom-cast-interactions.md, section 2).
 *
 * Each region has a few reactions. A reaction is a clip from her library when the model has one
 * (`clips`, first available wins), else a procedural gesture (`gesture`), plus an expression held
 * for a moment and a short voice-line key the host may speak (only when unmuted).
 *
 * - Head: a second tap within 1.6 s is a **pat**; pats escalate (happy → eyes closed → bounce).
 * - Four taps on the same other region within 4 s: she has had **enough** (sigh, sulk).
 * - Otherwise the region's reactions rotate, never the same one twice in a row.
 */
import type { Expression } from '../types';
import type { Region, RegionHit, Side } from './regions';

/** Clips never used for reactions (broken hands in the live model). */
export const BLOCKED_CLIPS = new Set(['idle_maid', 'bow', 'shy', 'sit_idle', 'think_chin']);

export interface Reaction {
  region: Region;
  side: Side;
  /** Reaction id, e.g. `head_tilt`, `pat_2`, `enough`. */
  name: string;
  /** Library clips to try, in order. */
  clips: string[];
  /** Procedural gesture when no clip is available (always available). */
  gesture: string;
  expression: Expression | null;
  weight: number;
  /** Seconds the expression is held before going back. */
  hold: number;
  /** Voice-line key for the host (e.g. `pat`, `poke_chest`). */
  line: string;
  /** Pat level 1..3 for head pats, else 0. */
  pat: number;
}

type Spec = Omit<Reaction, 'region' | 'side' | 'pat'>;

const S = (name: string, clips: string[], gesture: string, expression: Expression | null, weight: number, hold: number, line: string): Spec => ({
  name,
  clips,
  gesture,
  expression,
  weight,
  hold,
  line,
});

export const REACTIONS: Record<Region, Spec[]> = {
  head: [S('head_tilt', [], 'tilt', 'smile', 0.7, 1.8, 'head'), S('head_look', ['nod_yes'], 'nod', 'smile', 0.5, 1.6, 'head')],
  face: [S('face_poke', [], 'surprised', 'surprised', 0.8, 0.9, 'face'), S('face_giggle', [], 'giggle', 'smile', 0.9, 1.6, 'face')],
  chest: [S('chest_flinch', ['shake_no'], 'flinch', 'angry', 0.6, 1.8, 'chest'), S('chest_pout', [], 'pout', 'angry', 0.7, 2, 'chest')],
  hips: [S('hips_shy', [], 'shy', 'surprised', 0.5, 1.8, 'hips'), S('hips_flinch', [], 'flinch', 'angry', 0.5, 1.6, 'hips')],
  skirt: [S('skirt_shy', [], 'shy', 'surprised', 0.7, 2, 'skirt'), S('skirt_pout', ['shake_no'], 'pout', 'angry', 0.7, 2, 'skirt')],
  arm: [S('arm_look', [], 'look', 'smile', 0.5, 1.4, 'arm'), S('arm_nod', ['nod_yes'], 'nod', 'smile', 0.6, 1.4, 'arm')],
  hand: [S('hand_wave', ['greet_wave'], 'greet', 'smile', 0.9, 2.2, 'hand'), S('hand_present', ['present_open_palm'], 'nod', 'smile', 0.7, 1.8, 'hand')],
  leg: [S('leg_step', ['surprised_step'], 'surprised', 'surprised', 0.8, 1.2, 'leg'), S('leg_look', [], 'look', 'surprised', 0.5, 1.4, 'leg')],
};

export const PATS: Spec[] = [
  S('pat_1', [], 'pat', 'smile', 0.8, 2.2, 'pat'),
  S('pat_2', [], 'pat', 'smile', 1, 2.6, 'pat_more'),
  S('pat_3', ['happy_bounce', 'cheer'], 'happy', 'smile', 1, 3, 'pat_love'),
];

export const ENOUGH: Spec = S('enough', ['sigh'], 'pout', 'sad', 0.7, 2.6, 'enough');

export const PAT_WINDOW = 1.6;
export const ENOUGH_WINDOW = 4;
export const ENOUGH_COUNT = 4;

/** Every voice-line key a reaction can carry. */
export const LINE_KEYS = ['head', 'face', 'chest', 'hips', 'skirt', 'arm', 'hand', 'leg', 'pat', 'pat_more', 'pat_love', 'enough'] as const;
export type LineKey = (typeof LINE_KEYS)[number];

export class ReactionPlanner {
  private history: { region: Region; t: number }[] = [];
  private pats = 0;
  private lastHead = -Infinity;
  private last = new Map<Region, string>();

  constructor(private readonly random: () => number = Math.random) {}

  /** The reaction to a touch at time `t` (seconds). */
  plan(hit: RegionHit, t: number): Reaction {
    const { region, side } = hit;
    this.history = this.history.filter((h) => t - h.t <= ENOUGH_WINDOW);
    this.history.push({ region, t });
    if (region === 'head') {
      this.pats = t - this.lastHead <= PAT_WINDOW ? this.pats + 1 : 0;
      this.lastHead = t;
      if (this.pats > 0) {
        const level = Math.min(this.pats, PATS.length);
        return { ...PATS[level - 1]!, region, side, pat: level };
      }
    } else {
      this.pats = 0;
      this.lastHead = -Infinity;
      const same = this.history.filter((h) => h.region === region).length;
      if (same >= ENOUGH_COUNT) {
        this.history = this.history.filter((h) => h.region !== region);
        return { ...ENOUGH, region, side, pat: 0 };
      }
    }
    const options = REACTIONS[region];
    const fresh = options.filter((o) => o.name !== this.last.get(region));
    const pool = fresh.length ? fresh : options;
    const pick = pool[Math.min(pool.length - 1, Math.floor(this.random() * pool.length))]!;
    this.last.set(region, pick.name);
    return { ...pick, region, side, pat: 0 };
  }

  reset(): void {
    this.history = [];
    this.pats = 0;
    this.lastHead = -Infinity;
    this.last.clear();
  }
}
