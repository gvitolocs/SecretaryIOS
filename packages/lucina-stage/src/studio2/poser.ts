/**
 * IK pose editing (Custom Cast's IKManager / IKDragPoint) and saved poses.
 *
 * - `PoseLayer` holds a pose (local rotations by bone name, plus the hips position) and writes it
 *   over the clips every frame, so a saved pose can stand in as her idle.
 * - `PoseEditor` shows six handles (head, hands, feet, pelvis) and turns a drag into IK: hands and
 *   feet with two-bone IK (elbows back, knees forward), the head by aiming the neck and head, the
 *   pelvis by moving the hips while the feet stay planted.
 */
import {
  Group,
  Mesh,
  MeshBasicMaterial,
  Plane,
  Quaternion,
  Ray,
  SphereGeometry,
  Vector3,
  type Camera,
  type Object3D,
} from 'three';
import type { Humanoid, Side } from './bones';
import { aimWorld, rotateWorld, setWorldPosition, solveTwoBone, worldPos } from './ik';

export interface SavedPose {
  name: string;
  /** Local rotations [x, y, z, w] by bone name. */
  bones: Record<string, [number, number, number, number]>;
  /** Local position of the hips bone (by name), when the pelvis moved. */
  positions?: Record<string, [number, number, number]>;
}

export function cleanPose(p: unknown): SavedPose | null {
  const o = p as Partial<SavedPose> | null;
  if (!o || typeof o.name !== 'string' || !o.bones || typeof o.bones !== 'object') return null;
  const bones: SavedPose['bones'] = {};
  for (const [k, v] of Object.entries(o.bones)) {
    if (Array.isArray(v) && v.length === 4 && v.every((n) => Number.isFinite(n))) bones[k] = [v[0], v[1], v[2], v[3]];
  }
  const positions: NonNullable<SavedPose['positions']> = {};
  for (const [k, v] of Object.entries(o.positions ?? {})) {
    if (Array.isArray(v) && v.length === 3 && v.every((n) => Number.isFinite(n))) positions[k] = [v[0], v[1], v[2]];
  }
  return { name: o.name.slice(0, 40), bones, ...(Object.keys(positions).length ? { positions } : {}) };
}

export type HandleId = 'head' | 'hand_l' | 'hand_r' | 'foot_l' | 'foot_r' | 'pelvis';
export const HANDLES: readonly HandleId[] = ['head', 'hand_l', 'hand_r', 'foot_l', 'foot_r', 'pelvis'];
const HANDLE_COLOR: Record<HandleId, number> = { head: 0xffd34d, hand_l: 0xff6f9f, hand_r: 0xff6f9f, foot_l: 0x7fd6ff, foot_r: 0x7fd6ff, pelvis: 0x9be0c0 };

/** Writes a pose over whatever the clips did this frame. */
export class PoseLayer {
  pose: SavedPose | null = null;
  private rest = new Map<Object3D, { q: Quaternion; p: Vector3 }>();
  private nodes = new Map<string, Object3D>();
  private model: Object3D | null = null;
  private weight = 0;

  bind(model: Object3D): void {
    if (this.model === model) return;
    this.model = model;
    this.nodes.clear();
    model.traverse((o) => {
      if (o.name && !this.nodes.has(o.name)) this.nodes.set(o.name, o);
    });
  }

  node(name: string): Object3D | undefined {
    return this.nodes.get(name);
  }

  /** Applies the pose (eased in by `k` per frame, 1 = at once). */
  apply(k = 1): void {
    if (!this.pose) {
      this.weight = 0;
      return;
    }
    this.weight = Math.min(1, this.weight + k);
    const w = this.weight;
    for (const [name, q] of Object.entries(this.pose.bones)) {
      const b = this.nodes.get(name);
      if (!b) continue;
      if (!this.rest.has(b)) this.rest.set(b, { q: b.quaternion.clone(), p: b.position.clone() });
      const target = _q.set(q[0], q[1], q[2], q[3]);
      if (w >= 1) b.quaternion.copy(target);
      else b.quaternion.slerp(target, w);
    }
    for (const [name, p] of Object.entries(this.pose.positions ?? {})) {
      const b = this.nodes.get(name);
      if (!b) continue;
      if (!this.rest.has(b)) this.rest.set(b, { q: b.quaternion.clone(), p: b.position.clone() });
      if (w >= 1) b.position.set(p[0], p[1], p[2]);
      else b.position.lerp(_v.set(p[0], p[1], p[2]), w);
    }
  }

  /** Clears the pose and puts the bones it moved back where they were before it. */
  clear(): void {
    this.pose = null;
    this.weight = 0;
    for (const [b, r] of this.rest) {
      b.quaternion.copy(r.q);
      b.position.copy(r.p);
    }
    this.rest.clear();
  }

  set(pose: SavedPose | null, ease = true): void {
    if (!pose) return this.clear();
    this.pose = pose;
    if (!ease) this.weight = 1;
  }
}

const _q = new Quaternion();
const _v = new Vector3();

interface Drag {
  id: HandleId;
  plane: Plane;
  offset: Vector3;
  /** Head: the start direction from the head to the handle, and the start rotations. */
  headFrom?: Vector3;
  startLocal?: Map<Object3D, Quaternion>;
  /** Pelvis: start hips position and the planted feet. */
  hips0?: Vector3;
  handle0?: Vector3;
  feet?: Partial<Record<Side, { pos: Vector3; rot: Quaternion }>>;
}

/** The handles and the drag → IK logic. Works on the bones of one model at a time. */
export class PoseEditor {
  readonly group = new Group();
  private readonly handles = new Map<HandleId, Mesh>();
  private h: Humanoid | null = null;
  private height = 1.65;
  private headOffset = new Vector3();
  private footMin = -Infinity;
  private drag: Drag | null = null;
  /** Chains the user has moved since editing started (the head is only kept when dragged). */
  readonly touched = new Set<HandleId>();
  leftSign = 1;

  constructor() {
    this.group.name = 'PoseHandles';
    const geo = new SphereGeometry(1, 20, 14);
    for (const id of HANDLES) {
      const m = new Mesh(geo, new MeshBasicMaterial({ color: HANDLE_COLOR[id], transparent: true, opacity: 0.85, depthTest: false, depthWrite: false }));
      m.renderOrder = 999;
      m.name = `handle:${id}`;
      m.raycast = () => {};
      this.handles.set(id, m);
      this.group.add(m);
    }
    this.group.visible = false;
  }

  get dragging(): HandleId | null {
    return this.drag?.id ?? null;
  }

  bind(h: Humanoid, height: number, leftSign: number): void {
    this.h = h;
    this.height = height;
    this.leftSign = leftSign;
    // The head handle floats in front of her face (head local frame).
    if (h.head) {
      h.head.updateWorldMatrix(true, false);
      const p = worldPos(h.head).clone().add(new Vector3(0, 0.03 * height, 0.13 * height));
      this.headOffset.copy(h.head.worldToLocal(p));
    }
    const feet = [h.l.foot, h.r.foot].filter((f): f is Object3D => !!f).map((f) => worldPos(f).y);
    this.footMin = feet.length ? Math.min(...feet) - 0.01 * height : -Infinity;
    for (const id of HANDLES) this.handles.get(id)!.visible = !!this.boneFor(id);
  }

  private boneFor(id: HandleId): Object3D | undefined {
    const h = this.h;
    if (!h) return undefined;
    if (id === 'head') return h.head;
    if (id === 'pelvis') return h.hips;
    const side = id.endsWith('_l') ? 'l' : 'r';
    return id.startsWith('hand') ? h[side].hand : h[side].foot;
  }

  handlePos(id: HandleId, out = new Vector3()): Vector3 | null {
    const b = this.boneFor(id);
    if (!b) return null;
    if (id === 'head') {
      b.updateWorldMatrix(true, false);
      return b.localToWorld(out.copy(this.headOffset));
    }
    return worldPos(b, out);
  }

  /** Places the handles on the bones and sizes them for the camera (constant screen size). */
  update(camera: Camera): void {
    if (!this.group.visible) return;
    const cam = camera.getWorldPosition(new Vector3());
    for (const [id, m] of this.handles) {
      const p = this.handlePos(id, m.position);
      if (!p) continue;
      const big = this.drag?.id === id ? 1.35 : 1;
      m.scale.setScalar(Math.max(0.006, p.distanceTo(cam) * 0.017) * (id === 'pelvis' ? 1.2 : 1) * big);
    }
  }

  /** The handle nearest the screen point (CSS px) within `slop`, or null. */
  pick(camera: Camera, x: number, y: number, w: number, h: number, slop = 30): HandleId | null {
    let best: HandleId | null = null;
    let bestD = slop;
    const v = new Vector3();
    for (const id of HANDLES) {
      if (!this.handles.get(id)!.visible) continue;
      const p = this.handlePos(id, v);
      if (!p) continue;
      p.project(camera);
      if (p.z > 1) continue;
      const sx = ((p.x + 1) / 2) * w;
      const sy = ((1 - p.y) / 2) * h;
      const d = Math.hypot(sx - x, sy - y);
      if (d < bestD) {
        bestD = d;
        best = id;
      }
    }
    return best;
  }

  /** Starts dragging `id`; `ray` is the pointer ray. */
  begin(id: HandleId, ray: Ray, camera: Camera): boolean {
    const p = this.handlePos(id);
    const h = this.h;
    if (!p || !h) return false;
    const n = camera.getWorldDirection(new Vector3());
    const plane = new Plane().setFromNormalAndCoplanarPoint(n, p);
    const hit = ray.intersectPlane(plane, new Vector3());
    if (!hit) return false;
    const d: Drag = { id, plane, offset: p.clone().sub(hit) };
    if (id === 'head' && h.head) {
      d.headFrom = p.clone().sub(worldPos(h.head));
      d.startLocal = new Map([h.neck, h.head].filter((b): b is Object3D => !!b).map((b) => [b, b.quaternion.clone()]));
    }
    if (id === 'pelvis' && h.hips) {
      d.hips0 = worldPos(h.hips).clone();
      d.handle0 = p.clone();
      d.feet = {};
      for (const s of ['l', 'r'] as const) {
        const f = h[s].foot;
        if (f) d.feet[s] = { pos: worldPos(f).clone(), rot: f.getWorldQuaternion(new Quaternion()) };
      }
    }
    this.drag = d;
    this.touched.add(id);
    return true;
  }

  /** Moves the dragged handle along its plane and solves. */
  move(ray: Ray): void {
    const d = this.drag;
    const h = this.h;
    if (!d || !h) return;
    const hit = ray.intersectPlane(d.plane, new Vector3());
    if (!hit) return;
    const target = hit.add(d.offset);
    const H = this.height;
    switch (d.id) {
      case 'hand_l':
      case 'hand_r': {
        const s = d.id === 'hand_l' ? 'l' : 'r';
        const A = h[s];
        if (!A.upperArm || !A.lowerArm || !A.hand) return;
        const side = s === 'l' ? this.leftSign : -this.leftSign;
        solveTwoBone(A.upperArm, A.lowerArm, A.hand, target, new Vector3(side * 0.35, -0.25, -1).normalize());
        break;
      }
      case 'foot_l':
      case 'foot_r': {
        const s = d.id === 'foot_l' ? 'l' : 'r';
        const L = h[s];
        if (!L.upperLeg || !L.lowerLeg || !L.foot) return;
        // Keep the foot out of the floor.
        target.y = Math.max(target.y, this.footMin);
        solveTwoBone(L.upperLeg, L.lowerLeg, L.foot, target, new Vector3(0, 0, 1));
        break;
      }
      case 'head': {
        if (!h.head || !d.headFrom || !d.startLocal) return;
        for (const [b, q] of d.startLocal) b.quaternion.copy(q);
        h.head.updateMatrixWorld(true);
        const to = target.sub(worldPos(h.head));
        if (to.lengthSq() < 1e-10) return;
        // At most ~70° from where it started; the neck takes a third.
        const q = new Quaternion().setFromUnitVectors(d.headFrom.clone().normalize(), to.clone().normalize());
        const ang = d.headFrom.angleTo(to);
        const lim = (70 * Math.PI) / 180;
        if (ang > lim) q.slerp(new Quaternion(), 1 - lim / ang);
        if (h.neck) rotateWorld(h.neck, new Quaternion().slerp(q, 0.35));
        const want = d.headFrom.clone().applyQuaternion(q);
        const now = this.handlePos('head')!.sub(worldPos(h.head));
        aimWorld(h.head, now, want);
        break;
      }
      case 'pelvis': {
        if (!h.hips || !d.hips0 || !d.handle0) return;
        const delta = target.clone().sub(d.handle0);
        // Small range: the pelvis can sink (crouch) or shift, not fly.
        delta.clampLength(0, 0.45 * H);
        setWorldPosition(h.hips, d.hips0.clone().add(delta));
        for (const s of ['l', 'r'] as const) {
          const L = h[s];
          const f = d.feet?.[s];
          if (!L.upperLeg || !L.lowerLeg || !L.foot || !f) continue;
          solveTwoBone(L.upperLeg, L.lowerLeg, L.foot, f.pos, new Vector3(0, 0, 1));
          const pq = L.foot.parent!.getWorldQuaternion(new Quaternion()).invert();
          L.foot.quaternion.copy(pq.multiply(f.rot));
        }
        if (h.l.upperLeg) this.touched.add('foot_l');
        if (h.r.upperLeg) this.touched.add('foot_r');
        break;
      }
    }
  }

  end(): void {
    this.drag = null;
  }

  /** Bones of the chains a pose keeps (the head only once it was dragged). */
  poseBones(): Object3D[] {
    const h = this.h;
    if (!h) return [];
    const out: (Object3D | undefined)[] = [];
    for (const s of ['l', 'r'] as const) {
      out.push(h[s].upperArm, h[s].lowerArm, h[s].hand, h[s].upperLeg, h[s].lowerLeg, h[s].foot);
    }
    out.push(h.hips);
    if (this.touched.has('head')) out.push(h.neck, h.head);
    return out.filter((b): b is Object3D => !!b);
  }

  /** The current pose of the chains, by bone name. */
  capture(name: string): SavedPose {
    const bones: SavedPose['bones'] = {};
    for (const b of this.poseBones()) bones[b.name] = [b.quaternion.x, b.quaternion.y, b.quaternion.z, b.quaternion.w];
    const pose: SavedPose = { name, bones };
    const hips = this.h?.hips;
    if (hips) pose.positions = { [hips.name]: [hips.position.x, hips.position.y, hips.position.z] };
    return pose;
  }

  dispose(): void {
    this.group.removeFromParent();
    const first = this.handles.values().next().value as Mesh | undefined;
    first?.geometry.dispose();
    for (const m of this.handles.values()) (m.material as MeshBasicMaterial).dispose();
  }
}
