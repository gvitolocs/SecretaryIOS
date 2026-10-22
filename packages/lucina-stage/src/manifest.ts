/**
 * Avatar manifest: maps the logical morph/bone names the animator uses to the names in the model,
 * plus optional camera framing. Everything is optional; missing names fall back to well-known
 * candidates (MMD Japanese names, VRM names) and finally to "not bound", which the animator tolerates.
 */
import { LOGICAL_BONES, LOGICAL_MORPHS, type Framing, type LogicalBone, type LogicalMorph, type Vec3 } from './types';

export interface CameraFraming {
  /** Camera position in model space. */
  position?: Vec3;
  /** Look-at target in model space. */
  target?: Vec3;
  /** Vertical field of view in degrees. */
  fov?: number;
}

export interface NormalizedManifest {
  material?: string;
  toon?: unknown;
  /** Logical morph -> candidate model morph names, most specific first. */
  morphs: Partial<Record<LogicalMorph, string[]>>;
  /** Logical bone -> candidate model node names. */
  bones: Partial<Record<LogicalBone, string[]>>;
  camera: Partial<Record<Framing, CameraFraming>>;
  /** Mesh/node names to hide (e.g. a weapon or tear overlays). */
  hide: string[];
  /** Material names whose meshes are hidden. */
  hideMaterials: string[];
  /** Recommended idle pose: per-node world-space rotations (axis, degrees) and fixed morph weights. */
  idle: { rotations: Record<string, { axis: Vec3; angleDeg: number }>; morphs: Record<string, number> };
  /** Direction the character faces in the model's space. Auto-detected when absent. */
  facing?: '+z' | '-z';
  /** The raw `physics` block (secondary motion; validated by physics/config.ts). */
  physics?: unknown;
}

/** Fallback names, tried after anything the manifest lists. */
export const DEFAULT_MORPH_CANDIDATES: Record<LogicalMorph, string[]> = {
  blink: ['blink', 'Blink', 'まばたき', 'eyes_closed', 'Fcl_EYE_Close', 'eyeBlink'],
  blink_l: ['blink_l', 'Blink_L', 'blinkLeft', 'eyeBlinkLeft', 'ウィンク', 'Fcl_EYE_Close_L'],
  blink_r: ['blink_r', 'Blink_R', 'blinkRight', 'eyeBlinkRight', 'ウィンク右', 'Fcl_EYE_Close_R'],
  mouth_a: ['mouth_a', 'aa', 'あ', 'Fcl_MTH_A', 'vrc.v_aa', 'A'],
  mouth_i: ['mouth_i', 'ih', 'い', 'Fcl_MTH_I', 'vrc.v_ih', 'I'],
  mouth_u: ['mouth_u', 'ou', 'う', 'Fcl_MTH_U', 'vrc.v_ou', 'U'],
  mouth_e: ['mouth_e', 'ee', 'え', 'Fcl_MTH_E', 'vrc.v_e', 'E'],
  mouth_o: ['mouth_o', 'oh', 'お', 'Fcl_MTH_O', 'vrc.v_oh', 'O'],
  smile: ['smile', 'Smile', 'happy', 'joy', 'にこり', '笑い', 'Fcl_ALL_Joy'],
  sad: ['sad', 'Sad', 'sorrow', '困る', '悲しい', 'Fcl_ALL_Sorrow'],
  angry: ['angry', 'Angry', '怒り', 'Fcl_ALL_Angry'],
  surprised: ['surprised', 'Surprised', 'surprise', 'びっくり', '驚き', 'Fcl_ALL_Surprised'],
};

export const DEFAULT_BONE_CANDIDATES: Record<LogicalBone, string[]> = {
  spine: ['spine', 'Spine', '上半身', 'J_Bip_C_Spine', 'mixamorig:Spine'],
  chest: ['chest', 'Chest', 'upper_chest', 'UpperChest', '上半身2', '上半身3', 'J_Bip_C_Chest', 'mixamorig:Spine2'],
  neck: ['neck', 'Neck', '首', 'J_Bip_C_Neck', 'mixamorig:Neck'],
  head: ['head', 'Head', '頭', 'J_Bip_C_Head', 'mixamorig:Head'],
  eye_l: ['eye_l', 'Eye_L', 'eye.L', 'LeftEye', 'leftEye', '左目', 'J_Adj_L_FaceEye'],
  eye_r: ['eye_r', 'Eye_R', 'eye.R', 'RightEye', 'rightEye', '右目', 'J_Adj_R_FaceEye'],
  eyes: ['eyes', 'Eyes', '両目'],
  upper_arm_l: ['upper_arm_l', 'leftUpperArm', 'LeftUpperArm', 'upperArm.L', '左腕', 'J_Bip_L_UpperArm', 'mixamorig:LeftArm'],
  upper_arm_r: ['upper_arm_r', 'rightUpperArm', 'RightUpperArm', 'upperArm.R', '右腕', 'J_Bip_R_UpperArm', 'mixamorig:RightArm'],
  lower_arm_l: ['lower_arm_l', 'leftLowerArm', 'LeftLowerArm', 'forearm.L', '左ひじ', 'J_Bip_L_LowerArm', 'mixamorig:LeftForeArm'],
  lower_arm_r: ['lower_arm_r', 'rightLowerArm', 'RightLowerArm', 'forearm.R', '右ひじ', 'J_Bip_R_LowerArm', 'mixamorig:RightForeArm'],
};

const NAME_FIELDS = ['names', 'name', 'target', 'morph', 'bone', 'node', 'gltf_name', 'glb_name'];

function asNames(v: unknown): string[] {
  if (typeof v === 'string') return v ? [v] : [];
  if (Array.isArray(v)) return v.flatMap(asNames);
  if (v && typeof v === 'object') {
    const o = v as Record<string, unknown>;
    for (const k of NAME_FIELDS) {
      if (k in o) return asNames(o[k]);
    }
  }
  return [];
}

/** Like asNames, but an object entry without a name field means "the key is the name". */
function entryNames(key: string, v: unknown): string[] {
  if (v && typeof v === 'object' && !Array.isArray(v)) {
    const o = v as Record<string, unknown>;
    if (!NAME_FIELDS.some((k) => k in o)) {
      const orig = typeof o.original_name === 'string' ? [o.original_name] : [];
      return [key, ...orig];
    }
  }
  return asNames(v);
}

function asVec3(v: unknown): Vec3 | undefined {
  if (Array.isArray(v) && v.length >= 3 && v.slice(0, 3).every((n) => typeof n === 'number' && Number.isFinite(n))) {
    return [v[0] as number, v[1] as number, v[2] as number];
  }
  if (v && typeof v === 'object') {
    const o = v as Record<string, unknown>;
    if (typeof o.x === 'number' && typeof o.y === 'number' && typeof o.z === 'number') return [o.x, o.y, o.z];
  }
  return undefined;
}

function asFraming(v: unknown): CameraFraming | undefined {
  if (!v || typeof v !== 'object') return undefined;
  const o = v as Record<string, unknown>;
  const f: CameraFraming = {};
  const position = asVec3(o.position ?? o.eye ?? o.pos);
  const target = asVec3(o.target ?? o.lookAt ?? o.look_at ?? o.center);
  const fovRaw = o.fov ?? o.fov_y_deg ?? o.yfov_deg ?? o.fov_deg;
  if (position) f.position = position;
  if (target) f.target = target;
  if (typeof fovRaw === 'number' && fovRaw > 1 && fovRaw < 170) f.fov = fovRaw;
  else if (typeof o.yfov === 'number' && o.yfov > 0 && o.yfov < 3) f.fov = (o.yfov * 180) / Math.PI;
  return f.position || f.target || f.fov ? f : undefined;
}

const MORPH_KEY_ALIASES: Record<string, LogicalMorph> = {
  mouth_aa: 'mouth_a',
  aa: 'mouth_a',
  a: 'mouth_a',
  i: 'mouth_i',
  u: 'mouth_u',
  e: 'mouth_e',
  o: 'mouth_o',
  joy: 'smile',
  happy: 'smile',
  surprise: 'surprised',
  sorrow: 'sad',
  blink_left: 'blink_l',
  blink_right: 'blink_r',
};

const BONE_KEY_ALIASES: Record<string, LogicalBone> = {
  upper_chest: 'chest',
  left_eye: 'eye_l',
  right_eye: 'eye_r',
  eye_left: 'eye_l',
  eye_right: 'eye_r',
  'eye.l': 'eye_l',
  'eye.r': 'eye_r',
  both_eyes: 'eyes',
  eyes_both: 'eyes',
  upper_arm_left: 'upper_arm_l',
  upper_arm_right: 'upper_arm_r',
  lower_arm_left: 'lower_arm_l',
  lower_arm_right: 'lower_arm_r',
};

function logicalKey<T extends string>(key: string, all: readonly T[], aliases: Record<string, T>): T | undefined {
  const k = key.trim().toLowerCase();
  if ((all as readonly string[]).includes(k)) return k as T;
  return aliases[k];
}

/** Accepts the converter's manifest in a few plausible shapes; unknown fields are ignored. */
export function normalizeManifest(raw: unknown): NormalizedManifest {
  const out: NormalizedManifest = {
    morphs: {},
    bones: {},
    camera: {},
    hide: [],
    hideMaterials: [],
    idle: { rotations: {}, morphs: {} },
  };
  if (!raw || typeof raw !== 'object') return out;
  const m = raw as Record<string, unknown>;
  if (typeof m.material === 'string') out.material = m.material;
  if (m.toon !== undefined) out.toon = m.toon;
  if (m.physics !== undefined) out.physics = m.physics;

  const morphSrc = (m.morphs ?? m.morph_map ?? m.morphMap ?? m.morphTargets ?? m.blendshapes ?? m.expressions) as unknown;
  if (morphSrc && typeof morphSrc === 'object' && !Array.isArray(morphSrc)) {
    for (const [key, value] of Object.entries(morphSrc as Record<string, unknown>)) {
      const lk = logicalKey(key, LOGICAL_MORPHS, MORPH_KEY_ALIASES);
      if (!lk) continue;
      const names = entryNames(key, value);
      if (names.length) out.morphs[lk] = [...(out.morphs[lk] ?? []), ...names];
    }
  }

  // A dedicated logical->node map wins over a general bone list.
  const boneSources = [m.bone_names_for_stage, m.stage_bones, m.bone_map, m.boneMap, m.bones, m.skeleton];
  for (const boneSrc of boneSources) {
    if (!boneSrc || typeof boneSrc !== 'object' || Array.isArray(boneSrc)) continue;
    for (const [key, value] of Object.entries(boneSrc as Record<string, unknown>)) {
      // `eyes: { left, right }` (or l/r) is accepted as a pair.
      if (key.toLowerCase() === 'eyes' && value && typeof value === 'object' && !Array.isArray(value)) {
        const o = value as Record<string, unknown>;
        const l = asNames(o.left ?? o.l ?? o.L);
        const r = asNames(o.right ?? o.r ?? o.R);
        const both = asNames(o.both ?? o.name ?? o.names);
        if (l.length) out.bones.eye_l = l;
        if (r.length) out.bones.eye_r = r;
        if (both.length) out.bones.eyes = both;
        continue;
      }
      const lk = logicalKey(key, LOGICAL_BONES, BONE_KEY_ALIASES);
      if (!lk) continue;
      const names = entryNames(key, value);
      if (names.length) out.bones[lk] = [...(out.bones[lk] ?? []), ...names];
    }
  }

  const camSrc = (m.camera ?? m.cameras ?? m.camera_framing ?? m.framing) as unknown;
  if (camSrc && typeof camSrc === 'object') {
    const c = camSrc as Record<string, unknown>;
    for (const f of ['bust', 'full'] as const) {
      const fr = asFraming(c[f] ?? (f === 'full' ? c.full_body ?? c.fullBody : c.head_and_shoulders));
      if (fr) out.camera[f] = fr;
    }
    if (!out.camera.bust) {
      const single = asFraming(c);
      if (single) out.camera.bust = single;
    }
  }

  const hide = m.hide ?? m.hidden;
  if (Array.isArray(hide)) out.hide = hide.filter((h): h is string => typeof h === 'string');
  const hideMats = m.hide_materials ?? m.hideMaterials;
  if (Array.isArray(hideMats)) out.hideMaterials = hideMats.filter((h): h is string => typeof h === 'string');

  const idle = (m.recommended_idle ?? m.idle_pose ?? m.idle) as Record<string, unknown> | undefined;
  if (idle && typeof idle === 'object') {
    const rots = (idle.bone_local_rotations ?? idle.bones) as Record<string, unknown> | undefined;
    if (rots && typeof rots === 'object') {
      for (const [node, r] of Object.entries(rots)) {
        const o = r as Record<string, unknown>;
        const axis = asVec3(o?.world_axis);
        const angle = o?.world_angle_deg;
        if (axis && typeof angle === 'number' && Number.isFinite(angle)) out.idle.rotations[node] = { axis, angleDeg: angle };
      }
    }
    const morphs = idle.morphs as Record<string, unknown> | undefined;
    if (morphs && typeof morphs === 'object') {
      for (const [k, v] of Object.entries(morphs)) if (typeof v === 'number' && Number.isFinite(v)) out.idle.morphs[k] = v;
    }
  }

  const facing = m.facing ?? m.forward;
  if (typeof facing === 'string') {
    const f = facing.trim().toLowerCase();
    if (f.startsWith('+z')) out.facing = '+z';
    else if (f.startsWith('-z')) out.facing = '-z';
  }
  return out;
}

/** three.js GLTFLoader sanitises node names (PropertyBinding.sanitizeNodeName). */
export function sanitizeNodeName(name: string): string {
  return name.replace(/\s/g, '_').replace(/[[\]\.:\/]/g, '');
}

function looseKey(name: string): string {
  return name.toLowerCase().replace(/[\s_.\-:\/]/g, '');
}

/** Finds `wanted` in `available`: exact, then sanitised, then case/separator-insensitive. */
export function findName(wanted: string, available: readonly string[]): string | undefined {
  if (available.includes(wanted)) return wanted;
  const s = sanitizeNodeName(wanted);
  if (available.includes(s)) return s;
  const lk = looseKey(wanted);
  if (!lk) return undefined;
  return available.find((a) => looseKey(a) === lk);
}

export interface MeshMorphInfo {
  /** Mesh name (for diagnostics). */
  name: string;
  /** morphTargetDictionary of the mesh. */
  dictionary: Record<string, number>;
}

export interface MorphBinding {
  /** Index into the `meshes` array passed to `resolveMorphs`. */
  mesh: number;
  /** Morph target index on that mesh. */
  index: number;
  /** The model's morph name that matched. */
  name: string;
}

export interface MorphResolution {
  bindings: Partial<Record<LogicalMorph, MorphBinding[]>>;
  missing: LogicalMorph[];
}

function bindName(name: string, meshes: readonly MeshMorphInfo[]): MorphBinding[] {
  const found: MorphBinding[] = [];
  meshes.forEach((m, mi) => {
    const key = findName(name, Object.keys(m.dictionary));
    if (key !== undefined) {
      const index = m.dictionary[key];
      if (index !== undefined) found.push({ mesh: mi, index, name: key });
    }
  });
  return found;
}

/**
 * Resolves every logical morph to the targets that exist in the model. A morph name present on
 * several meshes (face, brows, lashes) binds all of them. `blink` falls back to blink_l + blink_r.
 */
export function resolveMorphs(manifest: NormalizedManifest, meshes: readonly MeshMorphInfo[]): MorphResolution {
  const bindings: Partial<Record<LogicalMorph, MorphBinding[]>> = {};
  for (const lm of LOGICAL_MORPHS) {
    const candidates = [...(manifest.morphs[lm] ?? []), ...DEFAULT_MORPH_CANDIDATES[lm]];
    for (const c of candidates) {
      const b = bindName(c, meshes);
      if (b.length) {
        bindings[lm] = b;
        break;
      }
    }
  }
  if (!bindings.blink && (bindings.blink_l || bindings.blink_r)) {
    bindings.blink = [...(bindings.blink_l ?? []), ...(bindings.blink_r ?? [])];
  }
  const missing = LOGICAL_MORPHS.filter((lm) => !bindings[lm]);
  return { bindings, missing };
}

export interface BoneResolution {
  bones: Partial<Record<LogicalBone, string>>;
  missing: LogicalBone[];
}

/** Resolves logical bones against the model's node names. */
export function resolveBones(manifest: NormalizedManifest, nodeNames: readonly string[]): BoneResolution {
  const bones: Partial<Record<LogicalBone, string>> = {};
  for (const lb of LOGICAL_BONES) {
    const candidates = [...(manifest.bones[lb] ?? []), ...DEFAULT_BONE_CANDIDATES[lb]];
    for (const c of candidates) {
      const hit = findName(c, nodeNames);
      if (hit !== undefined) {
        bones[lb] = hit;
        break;
      }
    }
  }
  // Never let chest and spine (or neck and head) resolve to the same node: the motion would double.
  if (bones.chest && bones.chest === bones.spine) delete bones.chest;
  if (bones.neck && bones.neck === bones.head) delete bones.neck;
  const missing = LOGICAL_BONES.filter((lb) => !bones[lb]);
  return { bones, missing };
}
