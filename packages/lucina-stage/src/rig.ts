/**
 * Binds logical bones/morphs to a three.js model and applies animator frames.
 * Bone deltas are expressed in character space (container space: faces +Z, Y up) and converted
 * into each bone's local space using the rest pose, so the procedural motion is independent of
 * the rig's bone-axis conventions.
 */
import { Euler, Mesh, Object3D, Quaternion, Vector3 } from 'three';
import type { AnimFrame } from './animator';
import {
  resolveBones,
  resolveMorphs,
  type MeshMorphInfo,
  type NormalizedManifest,
} from './manifest';
import { LOGICAL_MORPHS, type LogicalBone, type LogicalMorph, type RotDelta } from './types';

interface BoneBinding {
  node: Object3D;
  rest: Quaternion;
  /** Rest world rotation of the parent relative to the container, and its inverse. */
  parent: Quaternion;
  parentInv: Quaternion;
}

interface MorphTargetRef {
  influences: number[];
  index: number;
}

export interface RigReport {
  bones: Partial<Record<LogicalBone, string>>;
  missingBones: LogicalBone[];
  morphs: Partial<Record<LogicalMorph, string[]>>;
  missingMorphs: LogicalMorph[];
}

const _e = new Euler(0, 0, 0, 'YXZ');
const _d = new Quaternion();
const _q = new Quaternion();
const _p = new Quaternion();
const _pi = new Quaternion();
const _pw = new Quaternion();
const _cw = new Quaternion();

export class Rig {
  readonly bones: Partial<Record<LogicalBone, BoneBinding>> = {};
  readonly morphs: Partial<Record<LogicalMorph, MorphTargetRef[]>> = {};
  readonly meshes: Mesh[] = [];
  readonly report: RigReport;
  private readonly allTargets: MorphTargetRef[] = [];
  private readonly useEyePair: boolean;
  /** Static pose corrections applied under the animation (e.g. A-pose arms lowered). */
  private readonly offsets: Partial<Record<LogicalBone, Quaternion>> = {};
  /** Morphs held at a fixed weight (e.g. "sword off"), keyed by model morph name. */
  private readonly fixed: { ref: MorphTargetRef; weight: number }[] = [];
  /** Last clip-mixer pose of the clip-driven logical bones (see captureClipPose). */
  private readonly clipPose = new Map<Object3D, Quaternion>();

  constructor(
    /** The object whose space is "character space" (contains the model). */
    readonly container: Object3D,
    manifest: NormalizedManifest,
  ) {
    container.updateMatrixWorld(true);
    const containerInv = container.getWorldQuaternion(new Quaternion()).invert();

    const nodes = new Map<string, Object3D>();
    container.traverse((o) => {
      if (o.name && !nodes.has(o.name)) nodes.set(o.name, o);
      if ((o as Mesh).isMesh) this.meshes.push(o as Mesh);
    });

    const boneRes = resolveBones(manifest, [...nodes.keys()]);
    for (const [lb, name] of Object.entries(boneRes.bones) as [LogicalBone, string][]) {
      const node = nodes.get(name);
      if (!node) continue;
      const parent = node.parent
        ? containerInv.clone().multiply(node.parent.getWorldQuaternion(new Quaternion()))
        : new Quaternion();
      this.bones[lb] = { node, rest: node.quaternion.clone(), parent, parentInv: parent.clone().invert() };
    }
    this.useEyePair = !!(this.bones.eye_l || this.bones.eye_r);

    const morphMeshes = this.meshes.filter((m) => m.morphTargetDictionary && m.morphTargetInfluences);
    const infos: MeshMorphInfo[] = morphMeshes.map((m) => ({ name: m.name, dictionary: m.morphTargetDictionary! }));
    const morphRes = resolveMorphs(manifest, infos);
    const seen = new Set<string>();
    const reportMorphs: Partial<Record<LogicalMorph, string[]>> = {};
    for (const lm of LOGICAL_MORPHS) {
      const b = morphRes.bindings[lm];
      if (!b) continue;
      this.morphs[lm] = b.map((x) => {
        const mesh = morphMeshes[x.mesh]!;
        const ref = { influences: mesh.morphTargetInfluences!, index: x.index };
        const key = `${mesh.uuid}:${x.index}`;
        if (!seen.has(key)) {
          seen.add(key);
          this.allTargets.push(ref);
        }
        return ref;
      });
      reportMorphs[lm] = [...new Set(b.map((x) => x.name))];
    }
    this.report = {
      bones: boneRes.bones,
      missingBones: boneRes.missing,
      morphs: reportMorphs,
      missingMorphs: morphRes.missing,
    };
  }

  /** World position (in container space) of a logical bone, if bound. */
  bonePosition(b: LogicalBone, out = new Vector3()): Vector3 | null {
    const binding = this.bones[b];
    if (!binding) return null;
    this.container.updateMatrixWorld(true);
    binding.node.getWorldPosition(out);
    return this.container.worldToLocal(out);
  }

  /** Sets a static rotation offset (character space) for a bone, applied before the animation. */
  setOffset(b: LogicalBone, r: RotDelta | null): void {
    if (!r) delete this.offsets[b];
    else this.offsets[b] = new Quaternion().setFromEuler(new Euler(r.x, r.y, r.z, 'YXZ'));
  }

  /** Holds model morphs (by model name) at fixed weights. Returns the names that were found. */
  setFixedMorphs(weights: Record<string, number>): string[] {
    this.fixed.length = 0;
    const found: string[] = [];
    for (const [name, weight] of Object.entries(weights)) {
      for (const mesh of this.meshes) {
        const idx = mesh.morphTargetDictionary?.[name];
        if (idx === undefined || !mesh.morphTargetInfluences) continue;
        const ref = { influences: mesh.morphTargetInfluences, index: idx };
        this.fixed.push({ ref, weight });
        if (!this.allTargets.some((t) => t.influences === ref.influences && t.index === idx)) this.allTargets.push(ref);
        if (!found.includes(name)) found.push(name);
      }
    }
    return found;
  }

  /**
   * Puts the bound logical bones back to rest. Call before an AnimationMixer update when clips
   * play underneath, so bones a clip does not animate do not accumulate procedural deltas.
   */
  restoreBones(): void {
    for (const b of Object.values(this.bones)) if (b) b.node.quaternion.copy(this.clipPose.get(b.node) ?? b.rest);
  }

  /**
   * Call right after the clip mixer update. three's PropertyMixer only writes a value that changed since its last
   * write, so a constant track (a held forearm, two equal keys) is written once; restoreBones() then puts the bone
   * back to this pose instead of the rest pose, or the held bone would fall back to rest from the second frame on.
   */
  captureClipPose(driven: ReadonlySet<string>): void {
    for (const b of Object.values(this.bones)) {
      if (!b || !driven.has(b.node.name)) continue;
      let q = this.clipPose.get(b.node);
      if (!q) this.clipPose.set(b.node, (q = new Quaternion()));
      q.copy(b.node.quaternion);
    }
  }

  /**
   * Applies a procedural frame.
   * Default: deltas on top of the rest pose (procedural-only stage).
   * `layered`: deltas on top of the bones' current pose (set by a clip mixer this frame); the
   * parent frame is read live, static offsets are skipped for clip-driven bones, and the spine/chest
   * deltas (breathing, weight shift, lean) are scaled by `bodyGain` since clips carry their own.
   */
  apply(frame: AnimFrame, layered?: { driven: ReadonlySet<string>; bodyGain?: number }): void {
    let containerInv: Quaternion | null = null;
    if (layered) {
      this.container.updateMatrixWorld(true);
      containerInv = this.container.getWorldQuaternion(_cw).invert();
    }
    for (const [lb, binding] of Object.entries(this.bones) as [LogicalBone, BoneBinding][]) {
      if (lb === 'eyes' && this.useEyePair) continue;
      const r = frame.bones[lb];
      const driven = layered?.driven.has(binding.node.name) ?? false;
      const gain = layered && (lb === 'spine' || lb === 'chest') ? (layered.bodyGain ?? 0.35) : 1;
      _e.set(r.x * gain, r.y * gain, r.z * gain, 'YXZ');
      _d.setFromEuler(_e);
      const off = this.offsets[lb];
      if (off && !driven) _d.multiply(off);
      if (layered && binding.node.parent) {
        // local = parentInv * delta * parent * current, with the parent's posed rotation
        _p.copy(containerInv!).multiply(binding.node.parent.getWorldQuaternion(_pw));
        _pi.copy(_p).invert();
        _q.copy(_pi).multiply(_d).multiply(_p).multiply(driven ? binding.node.quaternion : binding.rest);
      } else {
        // local = parentInv * delta * parent * rest
        _q.copy(binding.parentInv).multiply(_d).multiply(binding.parent).multiply(binding.rest);
      }
      binding.node.quaternion.copy(_q);
    }
    for (const t of this.allTargets) t.influences[t.index] = 0;
    for (const f of this.fixed) f.ref.influences[f.ref.index] = f.weight;
    for (const lm of LOGICAL_MORPHS) {
      const refs = this.morphs[lm];
      if (!refs) continue;
      const w = frame.morphs[lm];
      if (!(w > 0)) continue;
      for (const t of refs) t.influences[t.index] = Math.min(1, (t.influences[t.index] ?? 0) + w);
    }
  }

  /** Restores the rest pose. */
  reset(): void {
    this.clipPose.clear();
    for (const b of Object.values(this.bones)) b?.node.quaternion.copy(b.rest);
    for (const t of this.allTargets) t.influences[t.index] = 0;
  }
}
