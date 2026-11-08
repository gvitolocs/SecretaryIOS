/**
 * Lucina's hairstyles and hair colour, independent of the outfit.
 *
 * - A hairstyle is a separate GLB part (`lucina-hair-<id>.glb`). Rigid parts are parented to the
 *   `head` bone at its bind pose (so a part exported in the model's rest space sits right, then
 *   follows every head turn); skinned parts are re-bound to the model's own bones by name.
 * - While a part is on, the base hair is hidden (materials `Hair`, `HairHighlight` and anything
 *   named like hair, or the entry's `hideMaterials`), and the tiara too when `hideTiara`.
 * - `natural` (or null) is the model's own hair.
 * - A colour tint recolours the hair that shows (base or part): the texture's luminance times
 *   the tint, injected into the material's shader; materials without the standard map chunk
 *   fall back to multiplying their colour.
 */
import {
  Color,
  Group,
  Matrix4,
  Skeleton,
  type Bone,
  type Material,
  type Mesh,
  type Object3D,
  type SkinnedMesh,
} from 'three';
import { tintToonMaterial, toonHandleFor, type ToonHandle } from './toon';
import { fixMaterials, loadModel, type LoadOptions } from './loader';

export const NATURAL_HAIR = 'natural';

export interface HairEntry {
  id: string;
  name?: Record<string, string> | string;
  /** Asset name of the part; absent only for `natural`. */
  glb?: string;
  thumbnail?: string;
  tags?: string[];
  /** Also hide the tiara while this style is on (buns, ponytails...). */
  hideTiara?: boolean;
  /** Base-model materials to hide instead of the default hair materials. */
  hideMaterials?: string[];
}

export interface HairColor {
  id: string;
  name?: Record<string, string> | string;
  /** `#rrggbb`. */
  hex: string;
}

const ID_RE = /^[a-z0-9][a-z0-9_-]{0,39}$/;
const ASSET_RE = /^[a-z0-9][a-z0-9._-]{0,127}$/;
const HEX_RE = /^#[0-9a-f]{6}$/i;
const BASE_HAIR = /^(?:hair|hairhighlight)$|hair|髪|kami/i;
const TIARA = /tiara|ティアラ|crown|circlet/i;

function names(raw: unknown): Record<string, string> | string | undefined {
  if (typeof raw === 'string' && raw.trim()) return raw.trim().slice(0, 80);
  if (!raw || typeof raw !== 'object') return undefined;
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(raw as Record<string, unknown>)) {
    if (/^[a-z]{2}$/.test(k) && typeof v === 'string' && v.trim()) out[k] = v.trim().slice(0, 80);
  }
  return Object.keys(out).length ? out : undefined;
}

function asset(v: unknown, exts: string[]): v is string {
  return typeof v === 'string' && ASSET_RE.test(v) && !v.includes('..') && exts.some((e) => v.endsWith(e));
}

/** Valid hairstyles of an untrusted list (unique ids, order kept; `natural` needs no glb). */
export function parseHairstyles(raw: unknown): HairEntry[] {
  if (!Array.isArray(raw)) return [];
  const out: HairEntry[] = [];
  const seen = new Set<string>();
  for (const r of raw) {
    if (!r || typeof r !== 'object') continue;
    const o = r as Record<string, unknown>;
    if (typeof o.id !== 'string' || !ID_RE.test(o.id) || seen.has(o.id)) continue;
    const natural = o.id === NATURAL_HAIR;
    if (!natural && !asset(o.glb, ['.glb'])) continue;
    const e: HairEntry = { id: o.id };
    if (!natural) e.glb = o.glb as string;
    const n = names(o.name);
    if (n) e.name = n;
    if (asset(o.thumbnail, ['.png', '.webp'])) e.thumbnail = o.thumbnail;
    if (Array.isArray(o.tags)) {
      const tags = o.tags.filter((t): t is string => typeof t === 'string' && t.length > 0 && t.length <= 32).slice(0, 16);
      if (tags.length) e.tags = tags;
    }
    if (o.hideTiara === true) e.hideTiara = true;
    if (Array.isArray(o.hideMaterials)) {
      const m = o.hideMaterials.filter((t): t is string => typeof t === 'string' && t.length > 0 && t.length <= 64).slice(0, 32);
      if (m.length) e.hideMaterials = m;
    }
    seen.add(e.id);
    out.push(e);
  }
  return out;
}

/** Valid colours of an untrusted list. */
export function parseHairColors(raw: unknown): HairColor[] {
  if (!Array.isArray(raw)) return [];
  const out: HairColor[] = [];
  const seen = new Set<string>();
  for (const r of raw) {
    if (!r || typeof r !== 'object') continue;
    const o = r as Record<string, unknown>;
    if (typeof o.id !== 'string' || !ID_RE.test(o.id) || seen.has(o.id) || typeof o.hex !== 'string' || !HEX_RE.test(o.hex)) continue;
    const c: HairColor = { id: o.id, hex: o.hex.toLowerCase() };
    const n = names(o.name);
    if (n) c.name = n;
    seen.add(c.id);
    out.push(c);
  }
  return out;
}

function materialsOf(m: Mesh): Material[] {
  return (Array.isArray(m.material) ? m.material : [m.material]).filter(Boolean) as Material[];
}

function meshesWhere(root: Object3D, test: (mats: Material[], mesh: Mesh) => boolean): Mesh[] {
  const out: Mesh[] = [];
  root.traverse((o) => {
    const m = o as Mesh;
    if (m.isMesh && test(materialsOf(m), m)) out.push(m);
  });
  return out;
}

interface TintUniforms {
  uHairTint: { value: Color };
  uHairTintAmount: { value: number };
}

/** Recolours `mat` (idempotent): luminance × tint, or back to the original with null. */
export function tintMaterial(mat: Material, hex: string | null): void {
  if (tintToonMaterial(mat, hex)) return;
  const m = mat as Material & { color?: Color; userData: Record<string, unknown> };
  const ud = m.userData;
  if (!ud.hairTint) {
    const uniforms: TintUniforms = { uHairTint: { value: new Color(1, 1, 1) }, uHairTintAmount: { value: 0 } };
    ud.hairTint = uniforms;
    ud.hairBaseColor = m.color?.clone();
    const prev = m.onBeforeCompile;
    m.onBeforeCompile = (shader, renderer) => {
      prev?.call(m, shader, renderer);
      if (!shader.fragmentShader.includes('#include <map_fragment>')) {
        ud.hairTintFallback = true;
        return;
      }
      Object.assign(shader.uniforms, uniforms);
      shader.fragmentShader = shader.fragmentShader
        .replace('void main() {', 'uniform vec3 uHairTint;\nuniform float uHairTintAmount;\nvoid main() {')
        .replace(
          '#include <map_fragment>',
          '#include <map_fragment>\n' +
            '  float hairLum = dot(diffuseColor.rgb, vec3(0.299, 0.587, 0.114));\n' +
            '  diffuseColor.rgb = mix(diffuseColor.rgb, uHairTint * (0.35 + 1.45 * hairLum), uHairTintAmount);',
        );
    };
    const prevKey = m.customProgramCacheKey?.bind(m);
    m.customProgramCacheKey = () => `${prevKey ? prevKey() : ''}|hairTint`;
    m.needsUpdate = true;
  }
  const u = ud.hairTint as TintUniforms;
  if (hex) {
    u.uHairTint.value.set(hex);
    u.uHairTintAmount.value = 1;
  } else {
    u.uHairTintAmount.value = 0;
  }
  // Materials whose shader has no map chunk: tint the colour instead.
  if (ud.hairTintFallback && m.color) {
    const base = (ud.hairBaseColor as Color | undefined) ?? new Color(1, 1, 1);
    m.color.copy(base);
    if (hex) m.color.multiply(new Color(hex)).multiplyScalar(1.6);
  }
}

/** Load a hairstyle part (materials fixed like the model's). */
export async function loadHairPart(url: string, opts: LoadOptions = {}): Promise<Object3D> {
  const { gltf } = await loadModel(url, opts);
  fixMaterials(gltf.scene);
  return gltf.scene;
}

/** Hair on one model: attach/detach parts, hide the base hair, tint. */
export class HairRig {
  private readonly baseHair: Mesh[];
  private readonly tiara: Mesh[];
  private readonly visibility = new Map<Mesh, boolean>();
  private partToon: ToonHandle | null = null;
  private part: Group | null = null;
  private entry: HairEntry | null = null;
  private tint: string | null = null;

  constructor(
    private readonly model: Object3D,
    private readonly headNode?: string,
  ) {
    this.baseHair = meshesWhere(model, (mats) => mats.some((m) => BASE_HAIR.test(m.name)));
    this.tiara = meshesWhere(model, (mats, mesh) => mats.some((m) => TIARA.test(m.name)) || TIARA.test(mesh.name));
    for (const m of [...this.baseHair, ...this.tiara]) this.visibility.set(m, m.visible);
  }

  get hairstyle(): string {
    return this.entry?.id ?? NATURAL_HAIR;
  }

  get hasBaseHair(): boolean {
    return this.baseHair.length > 0;
  }

  private headBone(): Object3D | null {
    if (!this.headNode) return null;
    let found: Object3D | null = null;
    this.model.traverse((o) => {
      if (!found && o.name === this.headNode) found = o;
    });
    return found;
  }

  /** The rigid part's local transform under the head bone: its bind pose. */
  private bindLocal(head: Object3D): Matrix4 {
    let local: Matrix4 | null = null;
    this.model.traverse((o) => {
      const sm = o as SkinnedMesh;
      if (local || !sm.isSkinnedMesh || !sm.skeleton) return;
      const i = sm.skeleton.bones.indexOf(head as Bone);
      if (i >= 0) local = sm.skeleton.boneInverses[i]!.clone().multiply(sm.bindMatrix);
    });
    if (local) return local;
    // No skin: from the current pose.
    this.model.updateMatrixWorld(true);
    return head.matrixWorld.clone().invert().multiply(this.model.matrixWorld);
  }

  /**
   * The model's joints by name. A node no skin of this model uses loads as a plain Object3D, not a
   * Bone (a hairstyle's own bones, grafted into every outfit so the part can be re-bound and its
   * spring chain can run); it drives a part's skin just as well. Bones win a name clash.
   */
  private bonesByName(): Map<string, Bone> {
    const out = new Map<string, Bone>();
    const nodes = new Map<string, Object3D>();
    this.model.traverse((o) => {
      if ((o as Bone).isBone) out.set(o.name, o as Bone);
      else if (o.name && !(o as Mesh).isMesh && o.children.every((c) => !(c as Mesh).isMesh)) nodes.set(o.name, o);
    });
    for (const [name, o] of nodes) if (!out.has(name)) out.set(name, o as Bone);
    return out;
  }

  /** Put `part` on for `entry` (null: back to the model's own hair). Synchronous. */
  attach(part: Object3D | null, entry: HairEntry | null): void {
    this.detach();
    if (!part || !entry || entry.id === NATURAL_HAIR) {
      this.applyTint();
      return;
    }
    this.partToon = toonHandleFor(part) ?? null;
    const group = new Group();
    group.name = `Hair:${entry.id}`;
    const skinned: SkinnedMesh[] = [];
    part.traverse((o) => {
      if ((o as SkinnedMesh).isSkinnedMesh && !o.userData.toonOutline) skinned.push(o as SkinnedMesh);
    });
    const bones = this.bonesByName();
    let rebound = 0;
    for (const sm of skinned) {
      const mapped = sm.skeleton?.bones.map((b) => bones.get(b.name));
      if (!mapped || mapped.some((b) => !b)) continue;
      sm.removeFromParent();
      sm.bind(new Skeleton(mapped as Bone[], sm.skeleton.boneInverses), sm.bindMatrix);
      group.add(sm);
      rebound++;
    }
    this.partToon?.update();
    const head = this.headBone();
    // Whatever was not re-bound (rigid meshes, or a skin whose bones the model lacks) rides on the head.
    let leftover = false;
    part.traverse((o) => {
      if ((o as Mesh).isMesh) leftover = true;
    });
    const rigid = leftover || rebound === 0 ? part : null;
    if (rigid) {
      const holder = new Group();
      holder.name = `HairRigid:${entry.id}`;
      holder.matrixAutoUpdate = false;
      if (head) holder.matrix.copy(this.bindLocal(head));
      holder.add(rigid);
      (head ?? this.model).add(holder);
      group.userData.rigid = holder;
    }
    if (group.children.length) this.model.add(group);
    this.part = group;
    this.entry = entry;
    const hide = entry.hideMaterials
      ? meshesWhere(this.model, (mats) => mats.some((m) => entry.hideMaterials!.includes(m.name)))
      : this.baseHair;
    for (const m of hide) {
      if (!this.visibility.has(m)) this.visibility.set(m, m.visible);
      m.visible = false;
    }
    if (entry.hideTiara) for (const m of this.tiara) m.visible = false;
    this.applyTint();
  }

  /** Remove the part and show the base hair again. */
  detach(): void {
    this.partToon?.dispose();
    this.partToon = null;
    for (const [m, v] of this.visibility) m.visible = v;
    if (this.part) {
      const rigid = this.part.userData.rigid as Object3D | undefined;
      for (const root of [this.part, rigid]) {
        if (!root) continue;
        root.removeFromParent();
        root.traverse((o) => {
          const m = o as Mesh;
          if (!m.isMesh) return;
          m.geometry?.dispose();
          for (const mat of materialsOf(m)) mat.dispose();
        });
      }
    }
    this.part = null;
    this.entry = null;
  }

  /** Hair colour (`#rrggbb`), null for the natural colour. */
  setTint(hex: string | null): void {
    this.tint = hex && HEX_RE.test(hex) ? hex : null;
    this.applyTint();
  }

  private applyTint(): void {
    const targets: Mesh[] = [];
    if (this.part) {
      const rigid = this.part.userData.rigid as Object3D | undefined;
      for (const root of [this.part, rigid]) if (root) targets.push(...meshesWhere(root, () => true));
    }
    // The base hair keeps its tint too (it is what shows without a part).
    targets.push(...this.baseHair);
    const seen = new Set<Material>();
    for (const m of targets) {
      for (const mat of materialsOf(m)) {
        if (seen.has(mat)) continue;
        seen.add(mat);
        tintMaterial(mat, this.tint);
      }
    }
  }

  dispose(): void {
    this.detach();
  }
}
