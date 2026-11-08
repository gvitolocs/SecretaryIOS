/**
 * Face sliders as morph targets generated at load from the model's own face geometry.
 *
 * G's face has no face bones in the GLB (20 baked morphs drive it), and re-exporting game data is
 * off limits, so the stage builds the extra targets itself: landmarks come from the geometry (eye
 * centres from the `EyeL`/`EyeR` primitives, the mouth from the vertices `mouth_i` moves, the brows
 * from the `Lash` primitive above each eye, chin, nose and cheeks from the face skin), and every
 * slider is a smooth local displacement field evaluated for every face primitive alike, so eyes,
 * lashes, brows and skin move together without cracks. Sizes are in units of half the eye spacing,
 * so the fields fit whatever the mesh's (quantised) space is.
 *
 * The targets are appended to each primitive's geometry and influence array (the toon outline hull
 * shares both), named `ed_<slider>`; the rig never touches them. Influences are signed (-1..1).
 */
import { BufferAttribute, Float32BufferAttribute, type BufferGeometry, type Material, type Mesh, type Object3D } from 'three';
import { FACE_SLIDERS, sliderFactor, type Look } from './look';

export type V3 = [number, number, number];

/** Materials of the face (G's names; `Mouth` on other variants). */
export const FACE_MATERIALS = ['EyeL', 'EyeR', 'EyeHL', 'Lash', 'FaceColor', 'FaceSkin', 'Mouth'];
export const MORPH_PREFIX = 'ed_';

export interface FacePart {
  mesh: Mesh;
  material: string;
}

export interface FaceLandmarks {
  /** Half the distance between the eye centres: the unit of every field. */
  u: number;
  eyeL: V3;
  eyeR: V3;
  /** Half width / half height of one eye (iris surface). */
  eyeHalf: [number, number];
  browL: V3;
  browR: V3;
  mouth: V3;
  mouthHalfWidth: number;
  chin: V3;
  nose: V3;
  /** Centre of the head's bounding box (geometry space). */
  centre: V3;
}

function materialName(m: Mesh): string {
  const mat = (Array.isArray(m.material) ? m.material[0] : m.material) as Material | undefined;
  return mat?.name ?? '';
}

/** The face primitives of a model (outline hulls excluded). */
export function findFaceParts(model: Object3D): FacePart[] {
  const out: FacePart[] = [];
  model.traverse((o) => {
    const m = o as Mesh;
    if (!m.isMesh || m.userData.toonOutline || !m.geometry?.attributes.position) return;
    const name = materialName(m);
    if (FACE_MATERIALS.includes(name)) out.push({ mesh: m, material: name });
  });
  return out;
}

const pos = (g: BufferGeometry, i: number): V3 => {
  const p = g.attributes.position!;
  return [p.getX(i), p.getY(i), p.getZ(i)];
};

function bbox(g: BufferGeometry): { min: V3; max: V3 } {
  const min: V3 = [Infinity, Infinity, Infinity];
  const max: V3 = [-Infinity, -Infinity, -Infinity];
  const n = g.attributes.position!.count;
  for (let i = 0; i < n; i++) {
    const p = pos(g, i);
    for (let k = 0; k < 3; k++) {
      min[k] = Math.min(min[k]!, p[k]!);
      max[k] = Math.max(max[k]!, p[k]!);
    }
  }
  return { min, max };
}

const mid = (b: { min: V3; max: V3 }): V3 => [(b.min[0] + b.max[0]) / 2, (b.min[1] + b.max[1]) / 2, (b.min[2] + b.max[2]) / 2];

/** Vertices moved by a morph (more than `frac` of its largest move): centroid and x extent. */
function moved(part: FacePart, morph: string, frac = 0.3): { c: V3; xMin: number; xMax: number; n: number } | null {
  const idx = part.mesh.morphTargetDictionary?.[morph];
  const target = idx === undefined ? undefined : part.mesh.geometry.morphAttributes.position?.[idx];
  if (!target) return null;
  const g = part.mesh.geometry;
  const rel = g.morphTargetsRelative;
  const d = (i: number): number => {
    const x = target.getX(i), y = target.getY(i), z = target.getZ(i);
    if (rel) return Math.hypot(x, y, z);
    const p = pos(g, i);
    return Math.hypot(x - p[0], y - p[1], z - p[2]);
  };
  let max = 0;
  for (let i = 0; i < target.count; i++) max = Math.max(max, d(i));
  if (max <= 0) return null;
  const c: V3 = [0, 0, 0];
  let n = 0, xMin = Infinity, xMax = -Infinity;
  for (let i = 0; i < target.count; i++) {
    if (d(i) <= frac * max) continue;
    const p = pos(g, i);
    c[0] += p[0]; c[1] += p[1]; c[2] += p[2];
    xMin = Math.min(xMin, p[0]);
    xMax = Math.max(xMax, p[0]);
    n++;
  }
  return n ? { c: [c[0] / n, c[1] / n, c[2] / n], xMin, xMax, n } : null;
}

/** Landmarks of the face, or null when the model has no recognisable eyes. */
export function faceLandmarks(parts: FacePart[]): FaceLandmarks | null {
  const of = (name: string) => parts.filter((p) => p.material === name);
  const eyeL = of('EyeL')[0], eyeR = of('EyeR')[0];
  if (!eyeL || !eyeR) return null;
  const bl = bbox(eyeL.mesh.geometry), br = bbox(eyeR.mesh.geometry);
  const cl = mid(bl), cr = mid(br);
  const u = Math.abs(cl[0] - cr[0]) / 2;
  if (!(u > 0)) return null;
  const eyeHalf: [number, number] = [
    (bl.max[0] - bl.min[0] + br.max[0] - br.min[0]) / 4,
    (bl.max[1] - bl.min[1] + br.max[1] - br.min[1]) / 4,
  ];
  const skins = [...of('FaceSkin'), ...of('FaceColor'), ...of('Mouth')];
  const skin = of('FaceSkin')[0] ?? skins[0];
  const head = skin ? bbox(skin.mesh.geometry) : { min: [cl[0] - 2 * u, cl[1] - 3 * u, cl[2] - 3 * u] as V3, max: [cl[0] + 2 * u, cl[1] + 2 * u, cl[2]] as V3 };
  const centre = mid(head);
  const eyeY = (cl[1] + cr[1]) / 2;
  const eyeZ = (cl[2] + cr[2]) / 2;
  const midX = (cl[0] + cr[0]) / 2;
  // Mouth: what `mouth_i` (lips spread) moves on the skin; else a guess below the eyes.
  let mouth: V3 = [midX, eyeY - 1.6 * u, eyeZ + 0.3 * u];
  let mouthHalfWidth = 0.5 * u;
  for (const p of skins) {
    const m = moved(p, 'mouth_i') ?? moved(p, 'mouth_u');
    if (m && m.n > 8) {
      mouth = m.c;
      mouthHalfWidth = Math.max(0.25 * u, 0.6 * Math.max(Math.abs(m.xMax - m.c[0]), Math.abs(m.c[0] - m.xMin)));
      break;
    }
  }
  // Brows: lash/brow vertices well above each eye.
  const brow = (c: V3): V3 => {
    const pts: V3[] = [];
    for (const p of of('Lash')) {
      const g = p.mesh.geometry;
      for (let i = 0; i < g.attributes.position!.count; i++) {
        const v = pos(g, i);
        if (v[1] > c[1] + eyeHalf[1] * 1.15 && Math.sign(v[0] - midX) === Math.sign(c[0] - midX) && Math.abs(v[0] - c[0]) < 1.1 * u) pts.push(v);
      }
    }
    if (!pts.length) return [c[0], c[1] + 0.55 * u, c[2] + 0.1 * u];
    const s = pts.reduce((a, v) => [a[0] + v[0], a[1] + v[1], a[2] + v[2]] as V3, [0, 0, 0] as V3);
    return [s[0] / pts.length, s[1] / pts.length, s[2] / pts.length];
  };
  // Chin: the lowest front skin vertex near the midline; nose: the frontmost one between eyes and mouth.
  let chin: V3 = [midX, mouth[1] - 0.9 * u, mouth[2] - 0.1 * u];
  let nose: V3 = [midX, (eyeY + mouth[1]) / 2, mouth[2] + 0.2 * u];
  if (skin) {
    const g = skin.mesh.geometry;
    let bestChin = Infinity, bestNose = -Infinity;
    for (let i = 0; i < g.attributes.position!.count; i++) {
      const v = pos(g, i);
      if (Math.abs(v[0] - midX) > 0.2 * u) continue;
      if (v[2] > mouth[2] - 0.6 * u && v[1] < mouth[1] && v[1] > mouth[1] - 2 * u && v[1] < bestChin) {
        bestChin = v[1];
        chin = v;
      }
      if (v[1] < eyeY - 0.25 * u && v[1] > mouth[1] + 0.2 * u && v[2] > bestNose) {
        bestNose = v[2];
        nose = v;
      }
    }
  }
  return { u, eyeL: cl, eyeR: cr, eyeHalf, browL: brow(cl), browR: brow(cr), mouth, mouthHalfWidth, chin, nose, centre };
}

const smooth = (e0: number, e1: number, x: number): number => {
  const t = Math.min(1, Math.max(0, (x - e0) / (e1 - e0)));
  return t * t * (3 - 2 * t);
};

/** 1 inside `hold` (normalised ellipsoid distance), easing to 0 at 1. */
function falloff(p: V3, c: V3, r: V3, hold = 0.45): number {
  const d = Math.hypot((p[0] - c[0]) / r[0], (p[1] - c[1]) / r[1], (p[2] - c[2]) / r[2]);
  return d >= 1 ? 0 : 1 - smooth(hold, 1, d);
}

export type Field = (p: V3) => V3;

/** The displacement at full strength (+1) of every face slider. */
export function faceFields(lm: FaceLandmarks): Record<string, Field> {
  const { u } = lm;
  const eyes: [V3, number][] = [
    [lm.eyeL, Math.sign(lm.eyeL[0] - lm.eyeR[0]) || 1],
    [lm.eyeR, -(Math.sign(lm.eyeL[0] - lm.eyeR[0]) || 1)],
  ];
  const brows: [V3, number][] = [
    [lm.browL, eyes[0]![1]],
    [lm.browR, eyes[1]![1]],
  ];
  const eyeR: V3 = [Math.max(0.7 * u, lm.eyeHalf[0] * 2.3), Math.max(0.6 * u, lm.eyeHalf[1] * 1.9), 0.7 * u];
  const eyeSum = (f: (p: V3, c: V3, side: number, w: number) => V3, r: V3 = eyeR, hold = 0.45): Field => (p) => {
    const out: V3 = [0, 0, 0];
    for (const [c, side] of eyes) {
      const w = falloff(p, c, r, hold);
      if (w <= 0) continue;
      const d = f(p, c, side, w);
      out[0] += d[0]; out[1] += d[1]; out[2] += d[2];
    }
    return out;
  };
  const rot = (p: V3, c: V3, a: number, w: number): V3 => {
    const x = p[0] - c[0], y = p[1] - c[1];
    const ca = Math.cos(a * w), sa = Math.sin(a * w);
    return [x * ca - y * sa - x, x * sa + y * ca - y, 0];
  };
  const browR: V3 = [0.62 * u, 0.26 * u, 0.6 * u];
  const mouthR: V3 = [lm.mouthHalfWidth * 1.9, 0.42 * u, 0.6 * u];
  const front = (p: V3) => smooth(lm.centre[2] - 0.6 * u, lm.centre[2] + 0.6 * u, p[2]);
  const eyeY = (lm.eyeL[1] + lm.eyeR[1]) / 2;
  const midX = (lm.eyeL[0] + lm.eyeR[0]) / 2;
  return {
    eyeSize: eyeSum((p, c, _s, w) => [(p[0] - c[0]) * 0.18 * w, (p[1] - c[1]) * 0.18 * w, 0]),
    eyeWidth: eyeSum((p, c, _s, w) => [(p[0] - c[0]) * 0.16 * w, 0, 0]),
    eyeHeight: eyeSum((p, c, _s, w) => [0, (p[1] - c[1]) * 0.16 * w, 0]),
    eyeSpacing: eyeSum((_p, _c, side, w) => [side * 0.07 * u * w, 0, 0], [eyeR[0] * 1.2, eyeR[1] * 1.1, eyeR[2]]),
    eyeY: eyeSum((_p, _c, _s, w) => [0, 0.08 * u * w, 0], [eyeR[0] * 1.1, eyeR[1] * 1.2, eyeR[2]]),
    eyeTilt: eyeSum((p, c, side, w) => rot(p, c, side * 0.16, w)),
    browY: (p) => {
      let dy = 0;
      for (const [c] of brows) dy += 0.09 * u * falloff(p, c, browR, 0.55);
      return [0, dy, 0];
    },
    browAngle: (p) => {
      const out: V3 = [0, 0, 0];
      for (const [c, side] of brows) {
        const w = falloff(p, c, browR, 0.55);
        if (w <= 0) continue;
        const d = rot(p, c, side * 0.22, w);
        out[0] += d[0]; out[1] += d[1];
      }
      return out;
    },
    mouthWidth: (p) => [(p[0] - lm.mouth[0]) * 0.2 * falloff(p, lm.mouth, mouthR, 0.5), 0, 0],
    mouthY: (p) => [0, 0.07 * u * falloff(p, lm.mouth, [mouthR[0], mouthR[1] * 1.3, mouthR[2]], 0.5), 0],
    faceWidth: (p) => {
      // Lower face: from the eyes (0) through the mouth (1) to the chin, fading under it.
      const down = smooth(eyeY + 0.1 * u, lm.mouth[1], p[1]);
      const up = smooth(lm.chin[1] - 0.45 * u, lm.chin[1] + 0.1 * u, p[1]);
      const w = (p[1] < lm.mouth[1] ? up : down) * front(p);
      return [-(p[0] - midX) * 0.075 * w, 0, 0];
    },
    jaw: (p) => {
      const w = falloff(p, lm.chin, [0.95 * u, 0.75 * u, 0.9 * u], 0.3) * smooth(lm.mouth[1] + 0.05 * u, lm.chin[1] + 0.2 * u, p[1]);
      return [0, -0.13 * u * w, 0.03 * u * w];
    },
    cheeks: (p) => {
      const out: V3 = [0, 0, 0];
      for (const [c, side] of eyes) {
        const cc: V3 = [c[0] + side * 0.15 * u, c[1] - 0.8 * u, c[2]];
        const w = falloff(p, cc, [0.7 * u, 0.55 * u, 0.8 * u], 0.3) * front(p);
        out[0] += side * 0.05 * u * w;
        out[2] += 0.035 * u * w;
      }
      return out;
    },
    noseY: (p) => [0, 0.06 * u * falloff(p, lm.nose, [0.3 * u, 0.32 * u, 0.4 * u], 0.4), 0],
  };
}

interface Bound {
  influences: number[];
  index: Record<string, number>;
}

/** The generated face sliders on one model. */
export class FaceMorpher {
  readonly landmarks: FaceLandmarks | null;
  private readonly bound: Bound[] = [];

  constructor(model: Object3D) {
    const parts = findFaceParts(model);
    this.landmarks = faceLandmarks(parts);
    if (!this.landmarks) return;
    const fields = faceFields(this.landmarks);
    const geoms = new Map<BufferGeometry, Record<string, number>>();
    for (const part of parts) {
      const mesh = part.mesh;
      const g = mesh.geometry;
      let index = geoms.get(g);
      if (!index) {
        index = addTargets(g, fields);
        geoms.set(g, index);
      }
      if (!mesh.morphTargetInfluences) mesh.morphTargetInfluences = [];
      if (!mesh.morphTargetDictionary) mesh.morphTargetDictionary = {};
      // Keep the same array (the rig and the outline hull hold references to it).
      const count = g.morphAttributes.position?.length ?? 0;
      while (mesh.morphTargetInfluences.length < count) mesh.morphTargetInfluences.push(0);
      for (const [name, i] of Object.entries(index)) mesh.morphTargetDictionary[MORPH_PREFIX + name] = i;
      this.bound.push({ influences: mesh.morphTargetInfluences, index });
    }
  }

  get ready(): boolean {
    return !!this.landmarks && this.bound.length > 0;
  }

  /** Sets every generated morph from the look's face sliders. */
  apply(look: Pick<Look, 'face'>): void {
    for (const s of FACE_SLIDERS) {
      const w = sliderFactor(s, look.face[s.id] ?? 50);
      for (const b of this.bound) {
        const i = b.index[s.id];
        if (i !== undefined) b.influences[i] = w;
      }
    }
  }
}

/** Appends one relative target per field to `g`; returns slider id → morph index. */
function addTargets(g: BufferGeometry, fields: Record<string, Field>): Record<string, number> {
  const p = g.attributes.position!;
  const n = p.count;
  const relative = g.morphTargetsRelative;
  if (!g.morphAttributes.position) {
    g.morphAttributes.position = [];
    g.morphTargetsRelative = true;
  }
  const targets = g.morphAttributes.position;
  const normals = g.morphAttributes.normal;
  const index: Record<string, number> = {};
  const rel = g.morphTargetsRelative || relative;
  for (const [id, field] of Object.entries(fields)) {
    const arr = new Float32Array(n * 3);
    for (let i = 0; i < n; i++) {
      const v: V3 = [p.getX(i), p.getY(i), p.getZ(i)];
      const d = field(v);
      arr[i * 3] = (rel ? 0 : v[0]) + d[0];
      arr[i * 3 + 1] = (rel ? 0 : v[1]) + d[1];
      arr[i * 3 + 2] = (rel ? 0 : v[2]) + d[2];
    }
    index[id] = targets.length;
    const attr = new Float32BufferAttribute(arr, 3);
    attr.name = MORPH_PREFIX + id;
    targets.push(attr);
    if (normals) {
      const nrm = g.attributes.normal;
      const base = new Float32Array(n * 3);
      if (!rel && nrm) for (let i = 0; i < n; i++) base.set([nrm.getX(i), nrm.getY(i), nrm.getZ(i)], i * 3);
      normals.push(new BufferAttribute(base, 3));
    }
  }
  return index;
}

const morphers = new WeakMap<Object3D, FaceMorpher>();

export function faceMorpherFor(model: Object3D): FaceMorpher {
  let m = morphers.get(model);
  if (!m) morphers.set(model, (m = new FaceMorpher(model)));
  return m;
}
