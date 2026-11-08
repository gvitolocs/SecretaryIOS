/**
 * Accessory slots (Custom Cast's `acchead`, `acckami`, `accmimi`, `megane`, `acckubi`, `accsenaka`,
 * `accude`…): glTF props on named bones.
 *
 * An accessory GLB (`acc-<id>.glb`) is authored in the model's rest space (metres, Y up, facing
 * +Z, the same space as `lucina.glb` at rest). A rigid prop is parented to its bone through the
 * bone's rest transform, so it sits where it was modelled and then follows the bone. A skinned prop
 * is re-bound to the model's bones by name. Bones the model lacks are grafted from the part's own
 * skeleton (spring bones of an outfit, say).
 *
 * Body sliders never stretch a prop: the skin deform (body.ts) does not touch bone transforms, and
 * only head and hands carry a (uniform) node scale, which a head or wrist prop should follow.
 */
import { Bone, Group, Matrix4, Skeleton, type Material, type Mesh, type Object3D, type SkinnedMesh } from 'three';
import { ACCESSORY_SLOTS, ID_RE, type AccessorySlot, type Label } from './look';

export interface AccessoryEntry {
  id: string;
  slot: AccessorySlot;
  /** Asset name, e.g. `acc-ribbon.glb`. */
  glb: string;
  name?: Partial<Label> | string;
  thumbnail?: string;
  /** Bone to ride on (default per slot). */
  bone?: string;
  /** Hides her tiara while worn (hats). */
  hideTiara?: boolean;
  credits?: string;
  licence?: string;
}

const ASSET_RE = /^[a-z0-9][a-z0-9._-]{0,127}$/;

/** Default bone per slot (G / VRM names). Wrist props ride on the hand: its origin is the wrist. */
export const SLOT_BONES: Record<AccessorySlot, string> = {
  head: 'head',
  hair: 'head',
  ears: 'head',
  eyes: 'head',
  neck: 'neck',
  back: 'chest',
  wristL: 'leftHand',
  wristR: 'rightHand',
};

/** Valid accessories of an untrusted list (unique ids, order kept). */
export function parseAccessories(raw: unknown): AccessoryEntry[] {
  if (!Array.isArray(raw)) return [];
  const out: AccessoryEntry[] = [];
  const seen = new Set<string>();
  for (const r of raw) {
    if (!r || typeof r !== 'object') continue;
    const o = r as Record<string, unknown>;
    if (typeof o.id !== 'string' || !ID_RE.test(o.id) || seen.has(o.id)) continue;
    if (typeof o.slot !== 'string' || !(ACCESSORY_SLOTS as readonly string[]).includes(o.slot)) continue;
    if (typeof o.glb !== 'string' || !ASSET_RE.test(o.glb) || o.glb.includes('..') || !o.glb.endsWith('.glb')) continue;
    const e: AccessoryEntry = { id: o.id, slot: o.slot as AccessorySlot, glb: o.glb };
    if (typeof o.name === 'string') e.name = o.name.slice(0, 80);
    else if (o.name && typeof o.name === 'object') {
      const n: Partial<Label> = {};
      for (const k of ['it', 'en', 'da'] as const) {
        const v = (o.name as Record<string, unknown>)[k];
        if (typeof v === 'string' && v.trim()) n[k] = v.trim().slice(0, 80);
      }
      if (Object.keys(n).length) e.name = n;
    }
    if (typeof o.thumbnail === 'string' && ASSET_RE.test(o.thumbnail) && /\.(png|webp)$/.test(o.thumbnail)) e.thumbnail = o.thumbnail;
    if (typeof o.bone === 'string' && /^[\w .:-]{1,64}$/.test(o.bone)) e.bone = o.bone;
    if (o.hideTiara === true) e.hideTiara = true;
    if (typeof o.credits === 'string') e.credits = o.credits.slice(0, 400);
    if (typeof o.licence === 'string') e.licence = o.licence.slice(0, 200);
    seen.add(e.id);
    out.push(e);
  }
  return out;
}

export function accessoryName(e: Pick<AccessoryEntry, 'id' | 'name'>, lang: string): string {
  if (typeof e.name === 'string') return e.name;
  const n = (e.name ?? {}) as Record<string, string>;
  return n[lang] ?? n.en ?? Object.values(n)[0] ?? e.id;
}

/** Re-binds the skinned meshes of `part` to `model`'s bones by name, grafting bones it lacks. */
export function rebindSkinned(model: Object3D, part: Object3D, into: Object3D): number {
  const bones = new Map<string, Bone>();
  model.traverse((o) => {
    if ((o as Bone).isBone && !bones.has(o.name)) bones.set(o.name, o as Bone);
  });
  const skinned: SkinnedMesh[] = [];
  part.traverse((o) => {
    if ((o as SkinnedMesh).isSkinnedMesh && !o.userData.toonOutline) skinned.push(o as SkinnedMesh);
  });
  let n = 0;
  for (const sm of skinned) {
    const mapped: Bone[] = [];
    let ok = true;
    for (const b of sm.skeleton?.bones ?? []) {
      let target = bones.get(b.name);
      if (!target) target = graft(b, bones) ?? undefined;
      if (!target) {
        ok = false;
        break;
      }
      mapped.push(target);
    }
    if (!ok || !mapped.length) continue;
    sm.removeFromParent();
    sm.bind(new Skeleton(mapped, sm.skeleton.boneInverses), sm.bindMatrix);
    into.add(sm);
    n++;
  }
  return n;
}

/** Moves a part's bone (and its bone children) under the model's bone of its parent's name. */
function graft(b: Bone, bones: Map<string, Bone>): Bone | null {
  const chain: Bone[] = [];
  let cur: Object3D | null = b;
  while (cur && (cur as Bone).isBone && !bones.has(cur.name)) {
    chain.unshift(cur as Bone);
    cur = cur.parent;
  }
  const anchor = cur && bones.get(cur.name);
  if (!anchor || !chain.length) return null;
  const top = chain[0]!;
  top.userData.editorGrafted = true;
  anchor.add(top); // local transform kept: same rest pose as the model
  top.traverse((o) => {
    if ((o as Bone).isBone && !bones.has(o.name)) bones.set(o.name, o as Bone);
  });
  return bones.get(b.name) ?? null;
}

interface Worn {
  entry: AccessoryEntry;
  holder: Group;
}

/** The props on one model. */
export class AccessoryRig {
  private readonly rest = new Map<string, Matrix4>();
  private readonly worn = new Map<AccessorySlot, Worn>();
  private readonly tiara: Mesh[] = [];
  private tiaraWanted = true;

  constructor(readonly model: Object3D) {
    // Rest transforms of the attach bones relative to the model, taken now (call at load, in the
    // rest pose; prepareModel does).
    model.updateMatrixWorld(true);
    const inv = model.matrixWorld.clone().invert();
    const names = new Set(Object.values(SLOT_BONES));
    model.traverse((o) => {
      if (names.has(o.name) && !this.rest.has(o.name)) this.rest.set(o.name, inv.clone().multiply(o.matrixWorld));
      const m = o as Mesh;
      if (m.isMesh && !m.userData.toonOutline) {
        const mats = (Array.isArray(m.material) ? m.material : [m.material]) as Material[];
        if (mats.some((x) => /tiara|ティアラ|circlet/i.test(x?.name ?? '')) || /tiara/i.test(m.name)) this.tiara.push(m);
      }
    });
  }

  private bone(name: string): Object3D | null {
    let found: Object3D | null = null;
    this.model.traverse((o) => {
      if (!found && o.name === name) found = o;
    });
    return found;
  }

  /** Rest matrix of a bone relative to the model (computed now if it was not captured). */
  private restOf(name: string, bone: Object3D): Matrix4 {
    let m = this.rest.get(name);
    if (!m) {
      this.model.updateMatrixWorld(true);
      m = this.model.matrixWorld.clone().invert().multiply(bone.matrixWorld);
      this.rest.set(name, m);
    }
    return m;
  }

  get slots(): Partial<Record<AccessorySlot, string>> {
    return Object.fromEntries([...this.worn].map(([s, w]) => [s, w.entry.id]));
  }

  has(slot: AccessorySlot, id: string): boolean {
    return this.worn.get(slot)?.entry.id === id;
  }

  /** Puts `part` (an accessory scene) on its slot, replacing what was there. Synchronous. */
  attach(entry: AccessoryEntry, part: Object3D): boolean {
    this.detach(entry.slot);
    const boneName = entry.bone ?? SLOT_BONES[entry.slot];
    const bone = this.bone(boneName);
    if (!bone) return false;
    const holder = new Group();
    holder.name = `Acc:${entry.slot}:${entry.id}`;
    holder.userData.editorAccessory = entry.id;
    const skinGroup = new Group();
    skinGroup.name = `AccSkin:${entry.id}`;
    rebindSkinned(this.model, part, skinGroup);
    if (skinGroup.children.length) {
      this.model.add(skinGroup);
      holder.userData.skin = skinGroup;
    }
    let rigid = false;
    part.traverse((o) => {
      if ((o as Mesh).isMesh) rigid = true;
    });
    if (rigid) {
      const pivot = new Group();
      pivot.matrixAutoUpdate = false;
      pivot.matrix.copy(this.restOf(boneName, bone)).invert();
      pivot.add(part);
      holder.add(pivot);
    }
    bone.add(holder);
    this.worn.set(entry.slot, { entry, holder });
    this.updateTiara();
    return true;
  }

  detach(slot: AccessorySlot): void {
    const w = this.worn.get(slot);
    if (!w) return;
    this.worn.delete(slot);
    for (const root of [w.holder, w.holder.userData.skin as Object3D | undefined]) {
      if (!root) continue;
      root.removeFromParent();
      root.traverse((o) => {
        const m = o as Mesh;
        if (!m.isMesh) return;
        m.geometry?.dispose();
        for (const mat of Array.isArray(m.material) ? m.material : [m.material]) mat?.dispose();
      });
    }
    this.updateTiara();
  }

  /** Her own tiara on/off (also off while a prop with `hideTiara` is worn). */
  setTiara(on: boolean): void {
    this.tiaraWanted = on;
    this.updateTiara();
  }

  private updateTiara(): void {
    const hidden = [...this.worn.values()].some((w) => w.entry.hideTiara);
    for (const m of this.tiara) {
      m.userData.editorTiaraVisible ??= m.visible;
      m.visible = this.tiaraWanted && !hidden ? (m.userData.editorTiaraVisible as boolean) : false;
    }
  }

  dispose(): void {
    for (const s of [...this.worn.keys()]) this.detach(s);
  }
}
