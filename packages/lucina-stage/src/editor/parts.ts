/**
 * Outfit mix and match (docs/design/avatar-parts.md).
 *
 * - The base model (`lucina-g-base.glb`, wardrobe outfit id `base`) has the body skin split into
 *   named region meshes (`Body_chest`, `Body_thigh_L`…) plus plain underwear.
 * - A part (`lucina-<outfit>-<slot>.glb`) is skinned meshes bound to G's bone names. It is re-bound
 *   to the model's bones by name; bones the model lacks (an outfit's skirt or tail spring bones)
 *   are grafted from the part.
 * - Wearing a part hides the body regions it lists (`hides`) and any part in a slot it
 *   `occupies` (a onepiece occupies top and bottom).
 *
 * Whole-outfit GLBs keep working unchanged: they are wardrobe outfits, swapped by the wardrobe.
 */
import { Group, type Mesh, type Object3D } from 'three';
import { rebindSkinned } from './accessories';
import { ID_RE, PART_SLOTS, type Label, type PartSlot } from './look';

export interface PartEntry {
  /** `<outfit>-<slot>`. */
  id: string;
  outfit: string;
  slot: PartSlot;
  glb: string;
  name?: Partial<Label> | string;
  thumbnail?: string;
  /** Body region meshes of the base to hide (`Body_*`). */
  hides: string[];
  /** Slots this part fills besides its own (a onepiece: top and bottom). */
  occupies: PartSlot[];
  /** Toon block for the part's materials (same keys as the manifest's `toon.materials`). */
  toon?: { materials?: Record<string, unknown> };
  credits?: string;
  licence?: string;
}

/** Id of the base outfit used for mix and match. */
export const BASE_OUTFIT = 'base';
const ASSET_RE = /^[a-z0-9][a-z0-9._-]{0,127}$/;
const REGION_RE = /^Body_[A-Za-z0-9_]{1,40}$/;

const isSlot = (v: unknown): v is PartSlot => typeof v === 'string' && (PART_SLOTS as readonly string[]).includes(v);

/** Valid parts of an untrusted list (unique ids, order kept). */
export function parseParts(raw: unknown): PartEntry[] {
  if (!Array.isArray(raw)) return [];
  const out: PartEntry[] = [];
  const seen = new Set<string>();
  for (const r of raw) {
    if (!r || typeof r !== 'object') continue;
    const o = r as Record<string, unknown>;
    if (typeof o.id !== 'string' || !ID_RE.test(o.id) || seen.has(o.id)) continue;
    if (typeof o.outfit !== 'string' || !ID_RE.test(o.outfit) || !isSlot(o.slot)) continue;
    if (typeof o.glb !== 'string' || !ASSET_RE.test(o.glb) || o.glb.includes('..') || !o.glb.endsWith('.glb')) continue;
    const e: PartEntry = {
      id: o.id,
      outfit: o.outfit,
      slot: o.slot,
      glb: o.glb,
      hides: Array.isArray(o.hides) ? o.hides.filter((h): h is string => typeof h === 'string' && REGION_RE.test(h)).slice(0, 40) : [],
      occupies: Array.isArray(o.occupies) ? o.occupies.filter(isSlot).filter((s) => s !== o.slot) : o.slot === 'onepiece' ? ['top', 'bottom'] : [],
    };
    if (o.slot === 'onepiece') for (const s of ['top', 'bottom'] as const) if (!e.occupies.includes(s)) e.occupies.push(s);
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
    if (o.toon && typeof o.toon === 'object' && !Array.isArray(o.toon)) {
      const m = (o.toon as Record<string, unknown>).materials;
      if (m && typeof m === 'object' && !Array.isArray(m)) e.toon = { materials: m as Record<string, unknown> };
    }
    if (typeof o.credits === 'string') e.credits = o.credits.slice(0, 400);
    if (typeof o.licence === 'string') e.licence = o.licence.slice(0, 200);
    seen.add(e.id);
    out.push(e);
  }
  return out;
}

/**
 * The slots of a selection after occupancy: later picks win, and a part that occupies a slot
 * removes what was there (picking a onepiece drops top and bottom; picking a top drops a onepiece).
 */
export function resolveParts(selection: Partial<Record<PartSlot, string>>, parts: PartEntry[], last?: PartSlot): Partial<Record<PartSlot, string>> {
  const out: Partial<Record<PartSlot, string>> = {};
  const order = PART_SLOTS.filter((s) => s !== last).concat(last ? [last] : []);
  for (const slot of order) {
    const id = selection[slot];
    const part = id ? parts.find((p) => p.id === id && p.slot === slot) : undefined;
    if (!part) continue;
    for (const [s, other] of Object.entries(out) as [PartSlot, string][]) {
      const o = parts.find((p) => p.id === other);
      if (part.occupies.includes(s) || o?.occupies.includes(slot)) delete out[s];
    }
    out[slot] = part.id;
  }
  return out;
}

/** The parts worn on one (base) model. */
export class PartsRig {
  private readonly worn = new Map<PartSlot, { entry: PartEntry; group: Group }>();
  private readonly regions = new Map<string, Mesh[]>();

  constructor(readonly model: Object3D) {
    model.traverse((o) => {
      const m = o as Mesh;
      if (!m.isMesh || m.userData.toonOutline) return;
      const key = m.name.match(/^(Body_[A-Za-z0-9_]+?)(?:_\d+)?$/)?.[1] ?? m.parent?.name.match(/^(Body_[A-Za-z0-9_]+)$/)?.[1];
      if (!key) return;
      const list = this.regions.get(key) ?? [];
      list.push(m);
      this.regions.set(key, list);
    });
  }

  /** Body region names of this model (tests). */
  get regionNames(): string[] {
    return [...this.regions.keys()];
  }

  get slots(): Partial<Record<PartSlot, string>> {
    return Object.fromEntries([...this.worn].map(([s, w]) => [s, w.entry.id]));
  }

  attach(entry: PartEntry, part: Object3D): boolean {
    for (const s of [entry.slot, ...entry.occupies]) this.detach(s);
    for (const [s, w] of [...this.worn]) if (w.entry.occupies.includes(entry.slot)) this.detach(s);
    const group = new Group();
    group.name = `Part:${entry.slot}:${entry.id}`;
    const n = rebindSkinned(this.model, part, group);
    if (!n) return false;
    this.model.add(group);
    this.worn.set(entry.slot, { entry, group });
    this.updateRegions();
    return true;
  }

  detach(slot: PartSlot): void {
    const w = this.worn.get(slot);
    if (!w) return;
    this.worn.delete(slot);
    w.group.removeFromParent();
    w.group.traverse((o) => {
      const m = o as Mesh;
      if (!m.isMesh) return;
      m.geometry?.dispose();
      for (const mat of Array.isArray(m.material) ? m.material : [m.material]) mat?.dispose();
    });
    this.updateRegions();
  }

  private updateRegions(): void {
    const hidden = new Set([...this.worn.values()].flatMap((w) => w.entry.hides));
    for (const [name, meshes] of this.regions) {
      for (const m of meshes) {
        m.userData.editorRegionVisible ??= m.visible;
        m.visible = hidden.has(name) ? false : (m.userData.editorRegionVisible as boolean);
      }
    }
  }

  dispose(): void {
    for (const s of [...this.worn.keys()]) this.detach(s);
  }
}
