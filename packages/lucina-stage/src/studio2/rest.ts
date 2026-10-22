/**
 * The rest (bind) pose of a skinned model, whatever a clip is doing right now: local rotations and
 * positions from the skeletons' inverse bind matrices. Bones outside any skeleton keep their
 * current transform.
 */
import { Matrix4, Quaternion, Vector3, type Object3D, type SkinnedMesh } from 'three';

export interface RestPose {
  q: Map<Object3D, Quaternion>;
  p: Map<Object3D, Vector3>;
}

export function restPose(model: Object3D): RestPose {
  const bindWorld = new Map<Object3D, Matrix4>();
  model.traverse((o) => {
    const sm = o as SkinnedMesh;
    if (!sm.isSkinnedMesh || !sm.skeleton) return;
    sm.skeleton.bones.forEach((b, i) => {
      if (bindWorld.has(b)) return;
      const inv = sm.skeleton.boneInverses[i];
      if (inv) bindWorld.set(b, inv.clone().invert());
    });
  });
  const q = new Map<Object3D, Quaternion>();
  const p = new Map<Object3D, Vector3>();
  const m = new Matrix4();
  const pos = new Vector3();
  const rot = new Quaternion();
  const scl = new Vector3();
  for (const [b, w] of bindWorld) {
    const pw = b.parent ? bindWorld.get(b.parent) : undefined;
    if (!pw) continue; // root of the skeleton: keep what it has
    m.copy(pw).invert().multiply(w);
    m.decompose(pos, rot, scl);
    q.set(b, rot.clone());
    p.set(b, pos.clone());
  }
  return { q, p };
}

/** Back-compat helper: rest local rotations only. */
export function restLocals(model: Object3D): Map<Object3D, Quaternion> {
  return restPose(model).q;
}

/** Runs `fn` with the bones in `rest` set to their rest pose, then restores the current pose. */
export function withRestPose<T>(model: Object3D, rest: RestPose, fn: () => T): T {
  const saved: [Object3D, Quaternion, Vector3][] = [];
  for (const [b, q] of rest.q) {
    saved.push([b, b.quaternion.clone(), b.position.clone()]);
    b.quaternion.copy(q);
    const p = rest.p.get(b);
    if (p) b.position.copy(p);
  }
  model.updateMatrixWorld(true);
  try {
    return fn();
  } finally {
    for (const [b, q, p] of saved) {
      b.quaternion.copy(q);
      b.position.copy(p);
    }
    model.updateMatrixWorld(true);
  }
}
