/**
 * Which part of her a tap hit.
 *
 * 1. The skin weights of the hit triangle say which bone moves that skin most; the bone's name
 *    (English, Mixamo/VRM, MMD Japanese, and this model's "leg left thigh" style) gives the region.
 * 2. Without a skinned hit or a recognisable bone name (placeholder, props, odd rigs), the hit
 *    height relative to her head and feet decides, with the hit's distance from her centre line
 *    telling arms from the torso.
 */
import type { BufferGeometry, Intersection, Object3D, SkinnedMesh } from 'three';

export type Region = 'head' | 'face' | 'chest' | 'hips' | 'skirt' | 'arm' | 'hand' | 'leg';
export const REGIONS: readonly Region[] = ['head', 'face', 'chest', 'hips', 'skirt', 'arm', 'hand', 'leg'];

export type Side = 'left' | 'right' | null;

export interface RegionHit {
  region: Region;
  side: Side;
  /** The bone that decided it, or null for the height fallback. */
  bone: string | null;
}

/** Her measurements in world space (rest pose), for the height fallback. */
export interface BodyMetrics {
  floorY: number;
  height: number;
  /** Head bone (base of the skull). */
  headY: number;
  /** Eye level. */
  eyeY: number;
  centerX: number;
  centerZ: number;
  /** Head bone depth: hits in front of it, at face height, are the face. */
  headZ: number;
  /** Half the shoulder width (metres). */
  shoulderHalf: number;
}

// Order matters: the first rule that matches wins (hand before arm, face before head…).
const RULES: [RegExp, Region][] = [
  [/skirt|スカート|裾|frill|dress_?(?:back|front|side)|coat_?tail|cape|マント/i, 'skirt'],
  [/eye|目|瞳|brow|眉|mouth|口|jaw|顎|tongue|舌|teeth|歯|cheek|頬|nose|nostril|鼻|(?:^|[^a-z])(?:top|bot|upper|lower)?lips?\d*(?:[^a-z]|$)|face(?!_?ctr)|顔/i, 'face'],
  [/hair|髪|head|頭|ear|耳|tiara|crown|ahoge|アホ毛|hat|帽/i, 'head'],
  [/neck|首(?!.*[手足])/i, 'head'],
  [/finger|thumb|index|middle|(?:^|[^a-z])ring(?:[^a-z]|$)|pinky|little|hand|wrist|palm|指|手首|手(?!首)|親指|人指|中指|薬指|小指/i, 'hand'],
  [/shoulder|clavicle|arm|elbow|肩|腕|ひじ|肘|袖|sleeve/i, 'arm'],
  [/thigh|knee|calf|shin|leg|foot|feet|ankle|toe|足|脚|ひざ|膝|もも|すね|つま先|靴|shoe|boot|sock/i, 'leg'],
  [/hip|pelvis|lower_?body|butt|waist|下半身|腰|尻|crotch/i, 'hips'],
  [/chest|breast|bust|spine|upper_?body|torso|collar|上半身|胸|乳|ribbon|neckline/i, 'chest'],
];

const LEFT = /(^|[^a-z])(l|left)([^a-z]|$)|left|左|_l$|\.l$|^l_/i;
const RIGHT = /(^|[^a-z])(r|right)([^a-z]|$)|right|右|_r$|\.r$|^r_/i;

/** Region of a bone name, or null when it says nothing (root, centre, physics helpers). */
export function regionOfBone(name: string): RegionHit | null {
  const n = name.replace(/([a-z])([A-Z])/g, '$1_$2');
  for (const [re, region] of RULES) {
    if (re.test(n)) {
      return { region, side: sideOf(n), bone: name };
    }
  }
  return null;
}

export function sideOf(name: string): Side {
  const l = LEFT.test(name);
  const r = RIGHT.test(name);
  if (l && !r) return 'left';
  if (r && !l) return 'right';
  return null;
}

/**
 * Region from where she was hit, in world space: above the eyes is head, the face down to the
 * chin, then chest, hips, legs. Far from the centre line at chest/hip height is an arm or a hand.
 * `facingX`: +1 if her left is +X in world space (she faces +Z), else −1.
 */
export function regionOfPoint(p: { x: number; y: number; z: number }, m: BodyMetrics, facingX = 1): RegionHit {
  const h = Math.max(m.height, 0.1);
  const dx = p.x - m.centerX;
  const side: Side = Math.abs(dx) < h * 0.02 ? null : (dx > 0) === facingX > 0 ? 'left' : 'right';
  const chinY = m.eyeY - h * 0.06;
  const shoulderY = m.headY - h * 0.06;
  const hipY = m.floorY + h * 0.52;
  const crotchY = m.floorY + h * 0.46;
  const out = Math.abs(dx);
  if (p.y >= m.eyeY + h * 0.012) return { region: 'head', side: null, bone: null };
  if (p.y >= chinY && out < h * 0.07) return { region: p.z >= m.headZ ? 'face' : 'head', side: null, bone: null };
  if (p.y >= hipY - h * 0.1 && out > m.shoulderHalf * 0.95) {
    return { region: p.y < hipY + h * 0.04 ? 'hand' : 'arm', side, bone: null };
  }
  if (p.y >= shoulderY - h * 0.2) return { region: p.y >= chinY ? 'head' : 'chest', side: null, bone: null };
  if (p.y >= crotchY) return { region: 'hips', side: null, bone: null };
  if (p.y >= crotchY - h * 0.12 && out < h * 0.12) return { region: 'skirt', side: null, bone: null };
  return { region: 'leg', side, bone: null };
}

/** The bone with the largest summed skin weight over the hit triangle's vertices. */
export function dominantBone(mesh: SkinnedMesh, face: { a: number; b: number; c: number }): string | null {
  const geo = mesh.geometry as BufferGeometry;
  const si = geo.getAttribute('skinIndex');
  const sw = geo.getAttribute('skinWeight');
  const bones = mesh.skeleton?.bones;
  if (!si || !sw || !bones?.length) return null;
  const sum = new Map<number, number>();
  for (const v of [face.a, face.b, face.c]) {
    for (let k = 0; k < si.itemSize; k++) {
      const w = sw.getComponent(v, k);
      if (w <= 0) continue;
      const b = si.getComponent(v, k);
      sum.set(b, (sum.get(b) ?? 0) + w);
    }
  }
  let best = -1;
  let bestW = 0;
  for (const [b, w] of sum) {
    if (w > bestW) {
      best = b;
      bestW = w;
    }
  }
  return best >= 0 ? (bones[best]?.name ?? null) : null;
}

/** Walks up from a bone until a name maps to a region (physics helpers hang under real bones). */
export function regionOfBoneChain(bone: Object3D | null | undefined): RegionHit | null {
  let o: Object3D | null | undefined = bone;
  for (let i = 0; o && i < 6; i++, o = o.parent) {
    const r = o.name ? regionOfBone(o.name) : null;
    if (r) return { ...r, bone: bone!.name };
  }
  return null;
}

const SKIRT = /skirt|スカート|dress|frill|裾/i;

function partName(hit: Intersection): string {
  const o = hit.object as { name?: string; material?: { name?: string } | { name?: string }[] };
  const mats = Array.isArray(o.material) ? o.material : [o.material];
  const idx = hit.face?.materialIndex ?? 0;
  const mat = Array.isArray(o.material) ? mats[idx] : mats[0];
  return `${o.name ?? ''} ${mat?.name ?? ''}`;
}

/** Region of a raycast hit on her: skin weights first, the mesh name second, height last. */
export function regionOfHit(hit: Intersection, m: BodyMetrics, facingX = 1): RegionHit {
  const mesh = hit.object as SkinnedMesh;
  if (mesh.isSkinnedMesh && hit.face) {
    const name = dominantBone(mesh, hit.face);
    if (name) {
      const bone = mesh.skeleton.bones.find((b) => b.name === name);
      const r = regionOfBoneChain(bone) ?? regionOfBone(name);
      if (r) {
        // Unnamed face joints hang under the head bone: in front, below the eyes, it is her face.
        if (r.region === 'head' && regionOfPoint(hit.point, m, facingX).region === 'face') r.region = 'face';
        // A skirt skinned to the thighs/pelvis: the material or mesh name tells.
        if ((r.region === 'hips' || r.region === 'leg') && SKIRT.test(partName(hit))) r.region = 'skirt';
        // MMD-style names without a side: take it from the hit point.
        if (!r.side && (r.region === 'arm' || r.region === 'hand' || r.region === 'leg')) {
          r.side = regionOfPoint(hit.point, m, facingX).side;
        }
        return r;
      }
    }
  }
  // A rigid part named after what it is (hair, ribbons…), unless the name is generic.
  const byMesh = hit.object.name ? regionOfBone(hit.object.name) : null;
  if (byMesh && byMesh.region !== 'chest') return { ...byMesh, bone: null };
  return regionOfPoint(hit.point, m, facingX);
}
