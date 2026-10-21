/**
 * Render quality tiers for the stage environment (sky, floor, particles, bloom).
 * `auto` picks a tier from the device and steps down at run time if frames are slow.
 */

export type StageQuality = 'low' | 'medium' | 'high';
export type StageQualitySetting = StageQuality | 'auto';
export type StageTheme = 'dark' | 'light';

export const STAGE_QUALITIES: readonly StageQualitySetting[] = ['auto', 'low', 'medium', 'high'];

export interface QualityProfile {
  /** Device pixel ratio cap. */
  dpr: number;
  /** Ambient motes around her. */
  particles: number;
  /** Octaves of nebula noise in the sky (0 = flat gradient). */
  skyDetail: number;
  bloom: boolean;
}

export const QUALITY_PROFILES: Record<StageQuality, QualityProfile> = {
  low: { dpr: 1, particles: 70, skyDetail: 2, bloom: false },
  medium: { dpr: 1.5, particles: 180, skyDetail: 3, bloom: false },
  high: { dpr: 2, particles: 300, skyDetail: 5, bloom: true },
};

export function isStageQuality(v: unknown): v is StageQualitySetting {
  return typeof v === 'string' && (STAGE_QUALITIES as readonly string[]).includes(v);
}

/** Picks a starting tier for `auto`: phones get medium, weak devices low, desktops high. */
export function detectQuality(env: {
  coarsePointer?: boolean;
  shortSide?: number;
  cores?: number;
  memoryGb?: number;
} = probeDevice()): StageQuality {
  const cores = env.cores ?? 8;
  const mem = env.memoryGb ?? 8;
  if (cores <= 2 || mem <= 2) return 'low';
  const phone = !!env.coarsePointer && (env.shortSide ?? 1000) < 820;
  if (phone) return cores <= 4 ? 'low' : 'medium';
  return cores <= 4 ? 'medium' : 'high';
}

function probeDevice(): { coarsePointer: boolean; shortSide: number; cores: number; memoryGb: number } {
  const g = globalThis as unknown as {
    matchMedia?: (q: string) => MediaQueryList;
    screen?: { width: number; height: number };
    navigator?: { hardwareConcurrency?: number; deviceMemory?: number };
  };
  let coarse = false;
  try {
    coarse = !!g.matchMedia?.('(pointer: coarse)').matches;
  } catch {
    coarse = false;
  }
  const s = g.screen;
  return {
    coarsePointer: coarse,
    shortSide: s ? Math.min(s.width, s.height) : 1000,
    cores: g.navigator?.hardwareConcurrency ?? 8,
    memoryGb: g.navigator?.deviceMemory ?? 8,
  };
}

/** One tier down, or null at the bottom. */
export function lowerQuality(q: StageQuality): StageQuality | null {
  return q === 'high' ? 'medium' : q === 'medium' ? 'low' : null;
}
