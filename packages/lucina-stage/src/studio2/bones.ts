/**
 * Finds humanoid bones on a model by name: the VRM/Unity humanoid names Lucina's G and B rigs use
 * (`leftUpperArm`, `leftIndexProximal`), plus Mixamo, VRoid (`J_Bip_L_…`), Biped (`Bip01_L_…`),
 * Rigify (`DEF-…​.L`) and MMD Japanese names. The rig's own logical-bone map wins when it has one.
 */
import type { Object3D } from 'three';

export type Side = 'l' | 'r';
export const FINGERS = ['thumb', 'index', 'middle', 'ring', 'little'] as const;
export type Finger = (typeof FINGERS)[number];
export type LimbJoint = 'shoulder' | 'upperArm' | 'lowerArm' | 'hand' | 'upperLeg' | 'lowerLeg' | 'foot' | 'toes';

export interface Humanoid {
  hips?: Object3D;
  spine?: Object3D;
  chest?: Object3D;
  neck?: Object3D;
  head?: Object3D;
  l: Partial<Record<LimbJoint, Object3D>>;
  r: Partial<Record<LimbJoint, Object3D>>;
  /** Three segments per finger, proximal to distal (missing segments left out). */
  fingers: Record<Side, Partial<Record<Finger, Object3D[]>>>;
}

/** Lower case, without separators and the usual rig prefixes. */
export function normName(name: string): string {
  return name
    .toLowerCase()
    .replace(/^mixamorig\d*:?/, '')
    .replace(/^(j_bip_|def-|bip01|bip001|valvebiped\.bip01_)/, '')
    .replace(/[\s_.:-]/g, '');
}

const SIDE_WORD: Record<Side, string[]> = { l: ['left', 'l'], r: ['right', 'r'] };
const JP_SIDE: Record<Side, string> = { l: '左', r: '右' };

/** Candidate normalised names for a side + stem list (prefix and suffix side forms). */
function sided(side: Side, stems: string[]): string[] {
  const out: string[] = [];
  for (const w of SIDE_WORD[side]) for (const s of stems) out.push(w + s, s + w);
  return out;
}

const LIMB_STEMS: Record<LimbJoint, string[]> = {
  shoulder: ['shoulder', 'clavicle', 'collarbone'],
  upperArm: ['upperarm', 'arm', 'uparm'],
  lowerArm: ['lowerarm', 'forearm', 'elbow'],
  hand: ['hand', 'wrist'],
  upperLeg: ['upperleg', 'upleg', 'thigh'],
  lowerLeg: ['lowerleg', 'leg', 'calf', 'shin', 'knee'],
  foot: ['foot', 'ankle'],
  toes: ['toes', 'toe', 'toebase', 'toe0'],
};
const LIMB_JP: Record<LimbJoint, string[]> = {
  shoulder: ['肩'],
  upperArm: ['腕'],
  lowerArm: ['ひじ'],
  hand: ['手首'],
  upperLeg: ['足'],
  lowerLeg: ['ひざ'],
  foot: ['足首'],
  toes: ['つま先'],
};
const FINGER_STEMS: Record<Finger, string[]> = {
  thumb: ['thumb', 'handthumb'],
  index: ['index', 'handindex', 'findex', 'indexfinger'],
  middle: ['middle', 'handmiddle', 'fmiddle', 'middlefinger'],
  ring: ['ring', 'handring', 'fring', 'ringfinger'],
  little: ['little', 'pinky', 'handpinky', 'fpinky', 'littlefinger'],
};
const FINGER_JP: Record<Finger, string> = { thumb: '親指', index: '人指', middle: '中指', ring: '薬指', little: '小指' };
const SEG_WORDS = [
  ['proximal', '1', '01', 'metacarpal'],
  ['intermediate', '2', '02'],
  ['distal', '3', '03'],
];

const CENTER: Record<'hips' | 'spine' | 'chest' | 'neck' | 'head', string[]> = {
  hips: ['hips', 'pelvis', 'hip', 'センター', '下半身'],
  spine: ['spine', '上半身'],
  chest: ['chest', 'spine1', 'spine2', 'upperchest', '上半身2'],
  neck: ['neck', '首'],
  head: ['head', '頭'],
};

/** Finds the humanoid bones of `model`; `logical` (the rig's bone map) is preferred where it has a name. */
export function findHumanoid(model: Object3D, logical: Partial<Record<string, string>> = {}): Humanoid {
  const byNorm = new Map<string, Object3D>();
  const byName = new Map<string, Object3D>();
  model.traverse((o) => {
    if (!o.name || (o as { isMesh?: boolean }).isMesh) return;
    if (!byName.has(o.name)) byName.set(o.name, o);
    const n = normName(o.name);
    if (!byNorm.has(n)) byNorm.set(n, o);
  });
  const pick = (cands: string[]): Object3D | undefined => {
    for (const c of cands) {
      const o = byNorm.get(c) ?? byName.get(c);
      if (o) return o;
    }
    return undefined;
  };
  const named = (k: string): Object3D | undefined => {
    const n = logical[k];
    return n ? byName.get(n) : undefined;
  };
  const h: Humanoid = { l: {}, r: {}, fingers: { l: {}, r: {} } };
  h.hips = pick(CENTER.hips);
  h.spine = named('spine') ?? pick(CENTER.spine);
  h.chest = named('chest') ?? pick(CENTER.chest);
  h.neck = named('neck') ?? pick(CENTER.neck);
  h.head = named('head') ?? pick(CENTER.head);
  for (const side of ['l', 'r'] as const) {
    const limbs = h[side];
    for (const j of Object.keys(LIMB_STEMS) as LimbJoint[]) {
      // Mixamo's "LeftLeg" is the shin and "LeftArm" the upper arm; check the specific stems first.
      const cands = [...sided(side, LIMB_STEMS[j]), ...LIMB_JP[j].map((s) => JP_SIDE[side] + s)];
      let o = pick(cands);
      if (j === 'upperArm') o = named(`upper_arm_${side}`) ?? o;
      if (j === 'lowerArm') o = named(`lower_arm_${side}`) ?? o;
      if (o) limbs[j] = o;
    }
    // "LeftLeg" may have matched both upper and lower leg: keep the hierarchy straight.
    if (limbs.upperLeg && limbs.lowerLeg === limbs.upperLeg) delete limbs.lowerLeg;
    if (limbs.upperArm && limbs.lowerArm === limbs.upperArm) delete limbs.lowerArm;
    for (const f of FINGERS) {
      const segs: Object3D[] = [];
      for (let i = 0; i < 3; i++) {
        const cands: string[] = [];
        for (const stem of FINGER_STEMS[f]) for (const seg of SEG_WORDS[i]!) cands.push(...sided(side, [stem + seg]));
        // MMD: 親指０/１/２, others １/２/３.
        const jp = f === 'thumb' ? ['０', '１', '２'][i]! : ['１', '２', '３'][i]!;
        cands.push(JP_SIDE[side] + FINGER_JP[f] + jp);
        const o = pick(cands);
        if (o) segs.push(o);
      }
      // Biped style: Finger0, Finger01, Finger02 (normalised: finger0, finger01, finger02).
      if (!segs.length) {
        const d = FINGERS.indexOf(f);
        for (const suffix of ['', '1', '2']) {
          const o = pick(sided(side, [`finger${d}${suffix}`]));
          if (o) segs.push(o);
        }
      }
      if (segs.length) h.fingers[side][f] = segs;
    }
  }
  return h;
}

/** Child bone of `b` that leads toward `toward` (or the first bone child). */
export function childToward(b: Object3D, toward?: Object3D): Object3D | undefined {
  if (toward) {
    let o: Object3D | null = toward;
    while (o && o.parent !== b) o = o.parent;
    if (o) return o;
  }
  return b.children.find((c) => !(c as { isMesh?: boolean }).isMesh);
}
