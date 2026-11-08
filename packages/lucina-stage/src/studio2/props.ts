/**
 * Props (Custom Cast's ObjectManager): add, move, rotate, scale and remove simple objects around
 * her, up to `MAX_PROPS`. The starter set is ten original props made in Blender
 * (tools/props/make_props.py → assets/props/lucina-props.glb): one top-level node per prop, origin
 * at the centre of its base. Flat colours become toon materials with an outline here.
 */
import {
  BackSide,
  Box3,
  Color,
  DataTexture,
  Group,
  Mesh,
  MeshBasicMaterial,
  MeshToonMaterial,
  NearestFilter,
  RedFormat,
  RingGeometry,
  ShaderMaterial,
  Vector3,
  type BufferGeometry,
  type Material,
  type MeshStandardMaterial,
  type Object3D,
  type Raycaster,
} from 'three';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';

export const PROP_KINDS = ['chair', 'table', 'cup', 'book', 'plant', 'lamp', 'cushion', 'umbrella', 'guitar', 'cat_plush'] as const;
export type PropKind = (typeof PROP_KINDS)[number];
export const MAX_PROPS = 12;
export const PROP_SCALE: [number, number] = [0.25, 3];
export const PROP_LIFT: [number, number] = [0, 1.5];

/** A placed prop, relative to her feet (metres, degrees). */
export interface PropState {
  id: string;
  kind: string;
  x: number;
  z: number;
  /** Height above the floor (e.g. a cup on the table). */
  y: number;
  rot: number;
  scale: number;
}

export function clampProp(p: PropState): PropState {
  const c = (v: number, lo: number, hi: number) => (Number.isFinite(v) ? Math.min(hi, Math.max(lo, v)) : lo);
  return {
    id: String(p.id),
    kind: String(p.kind),
    x: c(p.x, -6, 6),
    z: c(p.z, -6, 6),
    y: c(p.y, PROP_LIFT[0], PROP_LIFT[1]),
    rot: ((Number.isFinite(p.rot) ? p.rot : 0) % 360 + 360) % 360,
    scale: c(p.scale, PROP_SCALE[0], PROP_SCALE[1]),
  };
}

/** Where the n-th new prop goes: a ring of slots around her, in front first. */
export function defaultSlot(n: number, height: number): { x: number; z: number; rot: number } {
  const slots: [number, number][] = [
    [0.55, 0.35],
    [-0.55, 0.35],
    [0.75, -0.25],
    [-0.75, -0.25],
    [0.35, 0.7],
    [-0.35, 0.7],
    [0.9, 0.3],
    [-0.9, 0.3],
    [0.5, -0.7],
    [-0.5, -0.7],
    [0, 0.85],
    [0, -0.9],
  ];
  const [x, z] = slots[n % slots.length]!;
  const k = height / 1.65;
  return { x: x * k, z: z * k, rot: (-Math.atan2(x, z + 1.2) * 180) / Math.PI };
}

let gradient: DataTexture | null = null;
function toonGradient(): DataTexture {
  if (gradient) return gradient;
  const t = new DataTexture(new Uint8Array([90, 175, 255]), 3, 1, RedFormat);
  t.minFilter = t.magFilter = NearestFilter;
  t.generateMipmaps = false;
  t.needsUpdate = true;
  gradient = t;
  return t;
}

const OUTLINE_VS = /* glsl */ `
uniform float width;
void main() {
  vec3 p = position + normal * width;
  gl_Position = projectionMatrix * modelViewMatrix * vec4(p, 1.0);
}`;
const OUTLINE_FS = /* glsl */ `
uniform vec3 color;
void main() { gl_FragColor = vec4(color, 1.0); }`;

function outlineMaterial(width: number): ShaderMaterial {
  return new ShaderMaterial({
    uniforms: { width: { value: width }, color: { value: new Color(0x2a2233) } },
    vertexShader: OUTLINE_VS,
    fragmentShader: OUTLINE_FS,
    side: BackSide,
  });
}

/** Swaps the flat PBR colours for toon materials and adds an inverted-hull outline. */
export function toonify(root: Object3D, outline = 0.004): void {
  const meshes: Mesh[] = [];
  root.traverse((o) => {
    if ((o as Mesh).isMesh && !o.userData.outline) meshes.push(o as Mesh);
  });
  for (const m of meshes) {
    const swap = (mat: Material): Material => {
      const s = mat as MeshStandardMaterial;
      const toon = new MeshToonMaterial({ color: s.color ?? new Color(0xffffff), gradientMap: toonGradient() });
      if (s.emissive && s.emissiveIntensity > 0 && s.emissive.getHex() !== 0) {
        toon.emissive = s.emissive.clone();
        toon.emissiveIntensity = Math.min(2, s.emissiveIntensity);
      }
      toon.name = s.name;
      mat.dispose();
      return toon;
    };
    m.material = Array.isArray(m.material) ? m.material.map(swap) : swap(m.material);
    if (outline > 0 && m.geometry.attributes.normal) {
      const o = new Mesh(m.geometry, outlineMaterial(outline));
      o.userData.outline = true;
      o.name = `${m.name}:outline`;
      o.raycast = () => {};
      m.add(o);
    }
  }
}

export class PropLibrary {
  private readonly templates = new Map<string, Object3D>();
  private loading: Promise<void> | null = null;

  get kinds(): string[] {
    return [...this.templates.keys()];
  }

  get loaded(): boolean {
    return this.templates.size > 0;
  }

  /** Loads the props GLB once (later calls return the same promise). */
  load(url: string, fetchInit?: RequestInit): Promise<void> {
    if (this.loading) return this.loading;
    this.loading = (async () => {
      const loader = new GLTFLoader();
      if (fetchInit?.credentials === 'include') loader.setWithCredentials(true);
      const h = fetchInit?.headers;
      if (h && !(h instanceof Headers) && !Array.isArray(h)) loader.setRequestHeader(h as Record<string, string>);
      const gltf = await loader.loadAsync(url);
      this.addTemplates(gltf.scene);
    })();
    this.loading.catch(() => {
      this.loading = null;
    });
    return this.loading;
  }

  /** Registers each direct child of `root` as a prop template by name (tests, custom sets). */
  addTemplates(root: Object3D): void {
    for (const child of [...root.children]) {
      child.removeFromParent();
      child.position.set(0, 0, 0);
      toonify(child);
      this.templates.set(child.name, child);
    }
  }

  create(kind: string): Object3D | null {
    const t = this.templates.get(kind);
    if (!t) return null;
    // Geometry and materials are shared between copies (cheap; disposed with the library).
    const o = t.clone(true);
    o.name = `prop:${kind}`;
    return o;
  }

  dispose(): void {
    const geos = new Set<BufferGeometry>();
    const mats = new Set<Material>();
    for (const t of this.templates.values()) {
      t.traverse((o) => {
        const m = o as Mesh;
        if (!m.isMesh) return;
        geos.add(m.geometry);
        for (const mat of Array.isArray(m.material) ? m.material : [m.material]) mats.add(mat);
      });
    }
    geos.forEach((g) => g.dispose());
    mats.forEach((m) => m.dispose());
    this.templates.clear();
    this.loading = null;
  }
}

interface Placed {
  state: PropState;
  obj: Object3D;
}

let seq = 0;
export function propId(): string {
  seq = (seq + 1) % 1e6;
  return `p${Date.now().toString(36)}${seq.toString(36)}`;
}

/**
 * The props in the scene. `origin` is her feet (world); the group follows whichever scene the
 * current stage renders (outfit swaps make a new scene).
 */
export class PropSet {
  readonly group = new Group();
  private readonly placed = new Map<string, Placed>();
  private readonly ring: Mesh;
  private selectedId: string | null = null;
  private height = 1.65;
  readonly origin = new Vector3();
  onChange: (() => void) | null = null;

  constructor(private readonly lib: PropLibrary) {
    this.group.name = 'StudioProps';
    const ringMat = new MeshBasicMaterial({ color: 0xff7fb0, transparent: true, opacity: 0.85, depthWrite: false, depthTest: false });
    this.ring = new Mesh(new RingGeometry(0.9, 1, 48), ringMat);
    this.ring.rotation.x = -Math.PI / 2;
    this.ring.renderOrder = 998;
    this.ring.visible = false;
    this.ring.raycast = () => {};
    this.group.add(this.ring);
  }

  setFrame(origin: Vector3, height: number): void {
    this.origin.copy(origin);
    this.height = height;
    this.group.position.copy(origin);
  }

  get list(): PropState[] {
    return [...this.placed.values()].map((p) => ({ ...p.state }));
  }

  get count(): number {
    return this.placed.size;
  }

  get selected(): string | null {
    return this.selectedId;
  }

  get(id: string): PropState | null {
    const p = this.placed.get(id);
    return p ? { ...p.state } : null;
  }

  /** Adds a prop; null at the limit or for an unknown kind. */
  add(kind: string, at?: Partial<PropState>): PropState | null {
    if (this.placed.size >= MAX_PROPS) return null;
    const obj = this.lib.create(kind);
    if (!obj) return null;
    const slot = defaultSlot(this.placed.size, this.height);
    const state = clampProp({ id: at?.id ?? propId(), kind, x: slot.x, z: slot.z, y: 0, rot: slot.rot, scale: 1, ...at });
    obj.userData.propId = state.id;
    this.group.add(obj);
    this.placed.set(state.id, { state, obj });
    this.place(state.id);
    this.select(state.id);
    this.onChange?.();
    return { ...state };
  }

  update(id: string, patch: Partial<Omit<PropState, 'id' | 'kind'>>): void {
    const p = this.placed.get(id);
    if (!p) return;
    p.state = clampProp({ ...p.state, ...patch });
    this.place(id);
    this.onChange?.();
  }

  remove(id: string): void {
    const p = this.placed.get(id);
    if (!p) return;
    p.obj.removeFromParent();
    this.placed.delete(id);
    if (this.selectedId === id) this.select(null);
    this.onChange?.();
  }

  clear(): void {
    for (const id of [...this.placed.keys()]) this.remove(id);
  }

  /** Replaces everything with `states` (restoring a saved scene). Unknown kinds are skipped. */
  load(states: PropState[]): void {
    for (const p of this.placed.values()) p.obj.removeFromParent();
    this.placed.clear();
    this.select(null);
    for (const s of states.slice(0, MAX_PROPS)) {
      const obj = this.lib.create(s.kind);
      if (!obj) continue;
      const state = clampProp(s);
      obj.userData.propId = state.id;
      this.group.add(obj);
      this.placed.set(state.id, { state, obj });
      this.place(state.id);
    }
    this.select(null);
  }

  select(id: string | null): void {
    this.selectedId = id && this.placed.has(id) ? id : null;
    this.placeRing();
  }

  /** The prop under a ray, if any. */
  pick(ray: Raycaster): string | null {
    const objs = [...this.placed.values()].map((p) => p.obj);
    const hit = ray.intersectObjects(objs, true).find((h) => !h.object.userData.outline);
    let o: Object3D | null = hit?.object ?? null;
    while (o && o.userData.propId === undefined) o = o.parent;
    return (o?.userData.propId as string | undefined) ?? null;
  }

  private place(id: string): void {
    const p = this.placed.get(id);
    if (!p) return;
    const s = p.state;
    p.obj.position.set(s.x, s.y, s.z);
    p.obj.rotation.set(0, (s.rot * Math.PI) / 180, 0);
    p.obj.scale.setScalar(s.scale);
    if (id === this.selectedId) this.placeRing();
  }

  private placeRing(): void {
    const p = this.selectedId ? this.placed.get(this.selectedId) : undefined;
    if (!p) {
      this.ring.visible = false;
      return;
    }
    p.obj.updateMatrixWorld(true);
    const box = new Box3().setFromObject(p.obj);
    const size = box.getSize(new Vector3());
    const r = Math.max(size.x, size.z) * 0.6 + 0.04;
    this.ring.scale.setScalar(r);
    this.ring.position.set(p.state.x, 0.004, p.state.z);
    this.ring.visible = true;
  }

  dispose(): void {
    this.group.removeFromParent();
    this.ring.geometry.dispose();
    (this.ring.material as Material).dispose();
    this.placed.clear();
  }
}

