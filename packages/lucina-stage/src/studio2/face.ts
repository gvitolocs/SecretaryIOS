/**
 * Face morph sliders and saved faces (Custom Cast's FaceMorphList and
 * UserOriginalFacialExpressionUtil): every expression morph the model has, mixed on top of the
 * animator's own (blinks and lip-sync keep working), saved under a name.
 */
import type { Mesh, Object3D } from 'three';

export interface SavedFace {
  name: string;
  morphs: Record<string, number>;
}

/** Morphs that are switches, not expressions (the sword toggle, VRChat helpers). */
const SKIP = /off$|^vrc\.|^_|ファルシオン/i;

/** Expression morph names of the model, in model order, without duplicates. */
export function listMorphs(model: Object3D): string[] {
  const out: string[] = [];
  model.traverse((o) => {
    const m = o as Mesh;
    if (!m.isMesh || !m.morphTargetDictionary || !m.morphTargetInfluences) return;
    const entries = Object.entries(m.morphTargetDictionary).sort((a, b) => a[1] - b[1]);
    for (const [name] of entries) if (!SKIP.test(name) && !out.includes(name)) out.push(name);
  });
  return out;
}

export function cleanMorphs(w: Record<string, unknown>): Record<string, number> {
  const out: Record<string, number> = {};
  for (const [k, v] of Object.entries(w ?? {})) {
    const n = Number(v);
    if (typeof k === 'string' && k.length < 80 && Number.isFinite(n) && n > 0.001) out[k] = Math.min(1, n);
  }
  return out;
}

interface Ref {
  influences: number[];
  index: number;
  last: number;
}

/**
 * Writes slider weights every frame. A morph the rig also drives (it resets it each frame) gets
 * the slider added on top; a morph nobody else touches is simply set.
 */
export class FaceMixer {
  private refs = new Map<string, Ref[]>();
  private bound: Object3D | null = null;
  weights: Record<string, number> = {};

  bind(model: Object3D): void {
    if (this.bound === model) return;
    this.release();
    this.bound = model;
    this.refs.clear();
    model.traverse((o) => {
      const m = o as Mesh;
      if (!m.isMesh || !m.morphTargetDictionary || !m.morphTargetInfluences) return;
      for (const [name, index] of Object.entries(m.morphTargetDictionary)) {
        let list = this.refs.get(name);
        if (!list) this.refs.set(name, (list = []));
        list.push({ influences: m.morphTargetInfluences, index, last: Number.NaN });
      }
    });
  }

  apply(): void {
    for (const [name, list] of this.refs) {
      const w = this.weights[name] ?? 0;
      for (const r of list) {
        const cur = r.influences[r.index] ?? 0;
        if (!(w > 0)) {
          // Stop: undo our write on morphs nobody else drives.
          if (!Number.isNaN(r.last) && cur === r.last) r.influences[r.index] = 0;
          r.last = Number.NaN;
          continue;
        }
        const untouched = cur === r.last;
        const v = untouched ? w : Math.min(1, cur + w);
        r.influences[r.index] = v;
        r.last = v;
      }
    }
  }

  /** Zeroes what we set (on unbind). */
  release(): void {
    for (const list of this.refs.values()) {
      for (const r of list) {
        if (!Number.isNaN(r.last) && r.influences[r.index] === r.last) r.influences[r.index] = 0;
        r.last = Number.NaN;
      }
    }
  }
}
