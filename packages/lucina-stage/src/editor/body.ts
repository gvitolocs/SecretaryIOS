/**
 * Body sliders on a skinned model, without touching the GLB.
 *
 * Custom Cast scales `… _SCL_` twin bones: the skin is weighted to the twin, the children hang off
 * the real bone, so a thicker waist never stretches the chest and a prop on a scaled bone never
 * shears (docs/design/custom-cast-editor.md §2). Our models have no twins, so the stage does the
 * same thing at skinning time: a per-bone local matrix L is multiplied into that bone's skinning
 * matrix only (`world · L · boneInverse`), by wrapping `Skeleton.prototype.update` once. Nothing
 * else sees it: children, physics bones, clips, raycasts on bones and attached accessories keep
 * their transforms.
 *
 * - Lengths (neck, shoulders, arms, legs) stretch the skin along the bone and move the child
 *   bones by the same factor, so joints stay connected. The clips animate only rotations and the
 *   hips translation, so the moved positions survive every clip.
 * - Thickness (neck, waist, hips, arms, legs, bust) scales the skin across the bone.
 * - Twist/helper bones (`arm_left_shoulder_2_ctr`, `leg_left_thigh_ctr`…) get the same scale,
 *   expressed in the main bone's frame, so limbs scale evenly.
 * - Head and hands use real node scale (uniform), so the eyes, hair and props under them scale with
 *   them undistorted. Height scales the whole model; longer legs lift it, feet stay on the floor.
 */
import { Bone, Matrix4, Object3D, Skeleton, SkinnedMesh, Vector3 } from 'three';
import { BODY_SLIDERS, sliderFactor, type Look } from './look';

/** Local skin deform per bone (the bone's own frame); read by the patched Skeleton.update. */
const skinDeform = new WeakMap<Object3D, Matrix4>();
let patched = false;
const _m = new Matrix4();

/** Wrap Skeleton.update once: bones with a deform get `matrixWorld · L · boneInverse`. */
export function installSkinDeform(): void {
  if (patched) return;
  patched = true;
  const original = Skeleton.prototype.update;
  Skeleton.prototype.update = function update(this: Skeleton): void {
    original.call(this);
    const bones = this.bones;
    let touched = false;
    for (let i = 0; i < bones.length; i++) {
      const bone = bones[i];
      if (!bone) continue;
      const l = skinDeform.get(bone);
      if (!l) continue;
      _m.multiplyMatrices(bone.matrixWorld, l).multiply(this.boneInverses[i]!);
      _m.toArray(this.boneMatrices as Float32Array, i * 16);
      touched = true;
    }
    if (touched && this.boneTexture) this.boneTexture.needsUpdate = true;
  };
}

/** The deform currently set on a bone (tests, debugging). */
export function skinDeformOf(bone: Object3D): Matrix4 | undefined {
  return skinDeform.get(bone);
}

/** G's bone names; VRM-style humanoid names are the same. */
export const BODY_BONES = {
  hips: 'hips',
  lowerBody: 'lowerBody',
  spine: 'spine',
  chest: 'chest',
  neck: 'neck',
  head: 'head',
  shoulderL: 'leftShoulder',
  shoulderR: 'rightShoulder',
  upperArmL: 'leftUpperArm',
  upperArmR: 'rightUpperArm',
  lowerArmL: 'leftLowerArm',
  lowerArmR: 'rightLowerArm',
  handL: 'leftHand',
  handR: 'rightHand',
  upperLegL: 'leftUpperLeg',
  upperLegR: 'rightUpperLeg',
  lowerLegL: 'leftLowerLeg',
  lowerLegR: 'rightLowerLeg',
  footL: 'leftFoot',
  footR: 'rightFoot',
  breastL: 'breast_left',
  breastR: 'breast_right',
} as const;
type BoneKey = keyof typeof BODY_BONES;

const HELPER = /ctr|twist|roll|helper/i;

/** The resolved body factors of a look (slider → factor), clamped by the slider ranges. */
export function bodyFactors(look: Pick<Look, 'body'>): Record<string, number> {
  const out: Record<string, number> = {};
  for (const s of BODY_SLIDERS) out[s.id] = sliderFactor(s, look.body[s.id] ?? 50);
  return out;
}

interface Group {
  main: Bone;
  /** Helper bones skinned like the main one, with their rest transform relative to it. */
  helpers: { bone: Bone; rel: Matrix4; relInv: Matrix4 }[];
  /** Bones whose rest position is scaled by the group's length factor. */
  moved: { bone: Object3D; rest: Vector3 }[];
}

export class BodyDeformer {
  private readonly bones: Partial<Record<BoneKey, Bone>> = {};
  private readonly groups = new Map<string, Group>();
  private readonly restPos = new Map<Object3D, Vector3>();
  private readonly restScale = new Map<Object3D, Vector3>();
  private readonly modelRest: { scale: Vector3; y: number };
  private readonly legLength: number;
  private factors: Record<string, number> = {};

  constructor(readonly model: Object3D) {
    installSkinDeform();
    const byName = new Map<string, Bone>();
    model.traverse((o) => {
      if ((o as Bone).isBone && !byName.has(o.name)) byName.set(o.name, o as Bone);
    });
    for (const [k, name] of Object.entries(BODY_BONES) as [BoneKey, string][]) {
      const b = byName.get(name);
      if (b) this.bones[k] = b;
    }
    this.modelRest = { scale: model.scale.clone(), y: model.position.y };
    // Rest world (bind) matrices from the largest skin: valid whatever pose the model is in now.
    const bind = bindMatrices(model);
    const rel = (from: Bone, to: Bone): Matrix4 | null => {
      const a = bind.get(from);
      const b = bind.get(to);
      return a && b ? a.clone().invert().multiply(b) : null;
    };
    const group = (key: string, main: BoneKey, movesChildren: boolean): void => {
      const bone = this.bones[main];
      if (!bone) return;
      const helpers: Group['helpers'] = [];
      const moved: Group['moved'] = [];
      const walk = (b: Object3D, viaHelper: boolean): void => {
        for (const c of b.children) {
          if (!(c as Bone).isBone) continue;
          const helper = HELPER.test(c.name);
          if (helper) {
            const r = rel(c as Bone, bone);
            if (r) helpers.push({ bone: c as Bone, rel: r, relInv: r.clone().invert() });
            if (movesChildren && viaHelper) moved.push({ bone: c, rest: this.rest(c) });
            walk(c, true);
          } else if (movesChildren && !viaHelper) moved.push({ bone: c, rest: this.rest(c) });
        }
      };
      walk(bone, false);
      this.groups.set(key, { main: bone, helpers, moved });
    };
    group('neck', 'neck', true);
    group('spine', 'spine', false);
    group('hips', 'hips', false);
    group('lowerBody', 'lowerBody', false);
    for (const s of ['L', 'R'] as const) {
      group(`shoulder${s}`, `shoulder${s}`, true);
      group(`upperArm${s}`, `upperArm${s}`, true);
      group(`lowerArm${s}`, `lowerArm${s}`, true);
      group(`upperLeg${s}`, `upperLeg${s}`, true);
      group(`lowerLeg${s}`, `lowerLeg${s}`, true);
      group(`breast${s}`, `breast${s}`, false);
      for (const k of [`upperLeg${s}`, `head`, `hand${s}`] as BoneKey[]) {
        const b = this.bones[k];
        if (b) {
          this.rest(b);
          if (!this.restScale.has(b)) this.restScale.set(b, b.scale.clone());
        }
      }
    }
    const len = (k: BoneKey): number => (this.bones[k] ? this.rest(this.bones[k]!).length() : 0);
    const legs = [len('lowerLegL') + len('footL'), len('lowerLegR') + len('footR')].filter((l) => l > 0);
    this.legLength = legs.length ? legs.reduce((a, b) => a + b, 0) / legs.length : 0;
  }

  private rest(o: Object3D): Vector3 {
    let v = this.restPos.get(o);
    if (!v) this.restPos.set(o, (v = o.position.clone()));
    return v;
  }

  /** Bones found (tests). */
  get found(): string[] {
    return Object.keys(this.bones);
  }

  /** Applies the body sliders from the model's rest state (idempotent). */
  apply(look: Pick<Look, 'body'>): void {
    const f = (this.factors = bodyFactors(look));
    const deform = (key: string, sx: number, sy: number, sz: number, length = sy): void => {
      const g = this.groups.get(key);
      if (!g) return;
      const identity = sx === 1 && sy === 1 && sz === 1;
      const s = new Matrix4().makeScale(sx, sy, sz);
      if (identity) skinDeform.delete(g.main);
      else skinDeform.set(g.main, s);
      for (const h of g.helpers) {
        if (identity) skinDeform.delete(h.bone);
        else skinDeform.set(h.bone, h.rel.clone().multiply(s).multiply(h.relInv));
      }
      for (const m of g.moved) m.bone.position.copy(m.rest).multiplyScalar(length);
    };
    deform('neck', f.neckWidth!, f.neckLength!, f.neckWidth!);
    deform('spine', f.waist!, 1, f.waist!);
    deform('hips', f.hips!, 1, f.hips!);
    deform('lowerBody', f.hips!, 1, f.hips!);
    for (const s of ['L', 'R'] as const) {
      deform(`shoulder${s}`, 1, f.shoulders!, 1);
      deform(`upperArm${s}`, f.armThickness!, f.armLength!, f.armThickness!);
      deform(`lowerArm${s}`, f.armThickness!, f.armLength!, f.armThickness!);
      deform(`upperLeg${s}`, f.legThickness!, f.legLength!, f.legThickness!);
      deform(`lowerLeg${s}`, f.legThickness!, f.legLength!, f.legThickness!);
      deform(`breast${s}`, f.bust!, f.bust!, f.bust!);
      // Wider hips also set the legs a little apart.
      const leg = this.bones[`upperLeg${s}`];
      if (leg) {
        const r = this.rest(leg);
        leg.position.set(r.x * (1 + (f.hips! - 1) * 0.5), r.y, r.z);
      }
      const hand = this.bones[`hand${s}`];
      if (hand) hand.scale.copy(this.restScale.get(hand) ?? new Vector3(1, 1, 1)).multiplyScalar(f.handSize!);
    }
    const head = this.bones.head;
    if (head) head.scale.copy(this.restScale.get(head) ?? new Vector3(1, 1, 1)).multiplyScalar(f.head!);
    const h = f.height!;
    this.model.scale.copy(this.modelRest.scale).multiplyScalar(h);
    this.model.position.y = this.modelRest.y + (f.legLength! - 1) * this.legLength * this.modelRest.scale.y * h;
    this.model.updateMatrixWorld(true);
  }

  /** The factors last applied. */
  get current(): Readonly<Record<string, number>> {
    return this.factors;
  }

  /** Back to the model as loaded. */
  reset(): void {
    this.apply({ body: {} });
  }
}

/** Rest (bind-space) matrices of every bone, from the skin with the most bones. */
function bindMatrices(model: Object3D): Map<Object3D, Matrix4> {
  let best: SkinnedMesh | null = null;
  model.traverse((o) => {
    const s = o as SkinnedMesh;
    if (s.isSkinnedMesh && s.skeleton && (!best || s.skeleton.bones.length > best.skeleton.bones.length)) best = s;
  });
  const out = new Map<Object3D, Matrix4>();
  const skin = best as SkinnedMesh | null;
  if (skin) {
    skin.skeleton.bones.forEach((b, i) => {
      if (b) out.set(b, skin.skeleton.boneInverses[i]!.clone().invert());
    });
  }
  // Bones outside the skin: from the current world matrices relative to the model.
  model.updateMatrixWorld(true);
  const inv = model.matrixWorld.clone().invert();
  model.traverse((o) => {
    if ((o as Bone).isBone && !out.has(o)) out.set(o, inv.clone().multiply(o.matrixWorld));
  });
  return out;
}

const deformers = new WeakMap<Object3D, BodyDeformer>();

/** The model's body deformer (created on first use, at the model's current rest state). */
export function bodyDeformerFor(model: Object3D): BodyDeformer {
  let d = deformers.get(model);
  if (!d) deformers.set(model, (d = new BodyDeformer(model)));
  return d;
}
