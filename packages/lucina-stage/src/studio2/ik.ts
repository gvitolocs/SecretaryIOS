/**
 * Small IK toolkit on three.js bones (world-space math, local results):
 *
 * - `rotateWorld(bone, q)`: turn a bone by a world-space rotation about its own pivot;
 * - `solveTwoBone(a, b, c, target, pole)`: analytic two-bone IK (upper arm, forearm, hand or
 *   thigh, shin, foot). The end bone keeps its world orientation, so the hand or foot does not
 *   twist while the limb moves. Out-of-reach targets stretch the limb straight toward them;
 *   the bend stays in the current plane, or the pole's when the limb is straight;
 * - `aimWorld(bone, from, to)`: turn a bone so the world direction `from` points along `to`.
 */
import { Quaternion, Vector3, type Object3D } from 'three';

const _pq = new Quaternion();
const _pqi = new Quaternion();
const _q = new Quaternion();
const _a = new Vector3();
const _b = new Vector3();
const _c = new Vector3();
const _t = new Vector3();
const _ab = new Vector3();
const _ac = new Vector3();
const _at = new Vector3();
const _ba = new Vector3();
const _bc = new Vector3();
const _axis = new Vector3();
const _endWorld = new Quaternion();

/** Pre-multiplies the bone's world rotation by `q` (world space), updating its local quaternion. */
export function rotateWorld(bone: Object3D, q: Quaternion): void {
  if (bone.parent) {
    bone.parent.getWorldQuaternion(_pq);
    _pqi.copy(_pq).invert();
    // L' = P^-1 · q · P · L
    _q.copy(_pqi).multiply(q).multiply(_pq).multiply(bone.quaternion);
    bone.quaternion.copy(_q).normalize();
  } else bone.quaternion.premultiply(q).normalize();
  bone.updateMatrixWorld(true);
}

/** Sets the bone's world rotation (keeps its parent). */
export function setWorldQuaternion(bone: Object3D, world: Quaternion): void {
  if (bone.parent) {
    bone.parent.getWorldQuaternion(_pq);
    bone.quaternion.copy(_pq.invert().multiply(world)).normalize();
  } else bone.quaternion.copy(world);
  bone.updateMatrixWorld(true);
}

/** Turns `bone` so the world direction `from` (from its pivot) points along `to`. */
export function aimWorld(bone: Object3D, from: Vector3, to: Vector3, weight = 1): void {
  if (from.lengthSq() < 1e-12 || to.lengthSq() < 1e-12) return;
  const q = new Quaternion().setFromUnitVectors(from.clone().normalize(), to.clone().normalize());
  if (weight !== 1) q.slerp(new Quaternion(), 1 - weight);
  rotateWorld(bone, q);
}

const acos = (x: number): number => Math.acos(Math.min(1, Math.max(-1, x)));

/**
 * Two-bone IK: `a` (root), `b` (middle joint, a child of a), `c` (end, a child of b) reach for the
 * world `target`. `pole`: world direction the middle joint bends toward when the limb is straight.
 * Returns the distance left between the end and the target (0 when reached).
 */
export function solveTwoBone(a: Object3D, b: Object3D, c: Object3D, target: Vector3, pole: Vector3): number {
  a.updateMatrixWorld(true);
  c.getWorldQuaternion(_endWorld);
  a.getWorldPosition(_a);
  b.getWorldPosition(_b);
  c.getWorldPosition(_c);
  _t.copy(target);
  const lab = _b.distanceTo(_a);
  const lcb = _c.distanceTo(_b);
  if (lab < 1e-6 || lcb < 1e-6) return _c.distanceTo(_t);
  const eps = 1e-4 * (lab + lcb);
  const lat = Math.min(Math.max(_t.distanceTo(_a), Math.abs(lab - lcb) + eps), lab + lcb - eps);

  _ab.subVectors(_b, _a).normalize();
  _ac.subVectors(_c, _a).normalize();
  _ba.subVectors(_a, _b).normalize();
  _bc.subVectors(_c, _b).normalize();
  const acAb0 = acos(_ac.dot(_ab));
  const baBc0 = acos(_ba.dot(_bc));
  const acAb1 = acos((lcb * lcb - lab * lab - lat * lat) / (-2 * lab * lat));
  const baBc1 = acos((lat * lat - lab * lab - lcb * lcb) / (-2 * lab * lcb));

  // Bend plane: the current one, or the pole's when the limb is (nearly) straight.
  _axis.crossVectors(_ac, _ab);
  if (_axis.lengthSq() < 1e-6) _axis.crossVectors(_ac, pole);
  if (_axis.lengthSq() < 1e-10) _axis.set(1, 0, 0);
  _axis.normalize();
  // Straight limb, pole given: bend toward the pole (flip the axis if needed).
  if (acAb0 < 0.02 && pole.lengthSq() > 0) {
    const want = new Vector3().crossVectors(_ac, pole);
    if (want.lengthSq() > 1e-8 && want.dot(_axis) < 0) _axis.negate();
  }

  rotateWorld(a, new Quaternion().setFromAxisAngle(_axis, acAb1 - acAb0));
  rotateWorld(b, new Quaternion().setFromAxisAngle(_axis, baBc1 - baBc0));

  // Swing the whole limb so the end points at the target.
  c.getWorldPosition(_c);
  a.getWorldPosition(_a);
  _ac.subVectors(_c, _a);
  _at.subVectors(_t, _a);
  if (_ac.lengthSq() > 1e-12 && _at.lengthSq() > 1e-12) {
    rotateWorld(a, new Quaternion().setFromUnitVectors(_ac.normalize(), _at.normalize()));
  }
  setWorldQuaternion(c, _endWorld);
  c.getWorldPosition(_c);
  return _c.distanceTo(target);
}

/** World position of a bone (fresh). */
export function worldPos(o: Object3D, out = new Vector3()): Vector3 {
  o.updateWorldMatrix(true, false);
  return o.getWorldPosition(out);
}

/** Moves a bone so its world position becomes `p` (through its parent's inverse transform). */
export function setWorldPosition(o: Object3D, p: Vector3): void {
  o.updateWorldMatrix(true, false);
  const local = p.clone();
  if (o.parent) o.parent.worldToLocal(local);
  o.position.copy(local);
  o.updateMatrixWorld(true);
}
