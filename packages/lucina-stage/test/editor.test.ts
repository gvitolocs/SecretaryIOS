// @vitest-environment jsdom
import {
  Bone,
  BoxGeometry,
  BufferGeometry,
  Float32BufferAttribute,
  Group,
  Matrix4,
  Mesh,
  MeshStandardMaterial,
  Object3D,
  PlaneGeometry,
  Quaternion,
  Skeleton,
  SkinnedMesh,
  Uint16BufferAttribute,
  Vector3,
} from 'three';
import { describe, expect, it, vi } from 'vitest';
import {
  AccessoryRig,
  applyLook,
  BODY_SLIDERS,
  BodyDeformer,
  createAvatarEditor,
  defaultLook,
  FACE_SLIDERS,
  FaceMorpher,
  faceLandmarks,
  findFaceParts,
  hexToHsv,
  hsvToHex,
  LookHistory,
  looksEqual,
  makeupAt,
  makeupMask,
  normalizeLook,
  parseAccessories,
  parseParts,
  PartsRig,
  randomLook,
  resetCategory,
  resolveParts,
  rng,
  skinDeformOf,
  sliderFactor,
  type Look,
} from '../src/editor';
import type { LucinaStage } from '../src/stage';

// ---------------------------------------------------------------------------------------------
// Fixtures

const bone = (name: string, parent: Object3D | null, pos: [number, number, number]): Bone => {
  const b = new Bone();
  b.name = name;
  b.position.set(...pos);
  parent?.add(b);
  return b;
};

/** A small humanoid with G's bone names (Y along each bone), skinned to one box per limb. */
function rig(): { model: Group; bones: Record<string, Bone>; skin: SkinnedMesh } {
  const model = new Group();
  const hips = bone('hips', model, [0, 1, 0]);
  const lowerBody = bone('lowerBody', hips, [0, 0, 0]);
  const spine = bone('spine', hips, [0, 0.06, 0]);
  const chest = bone('chest', spine, [0, 0.14, 0]);
  const neck = bone('neck', chest, [0, 0.21, 0]);
  const head = bone('head', neck, [0, 0.07, 0]);
  const shoulder = bone('leftShoulder', chest, [0.02, 0.17, 0]);
  shoulder.rotation.z = -Math.PI / 2; // its Y points to +X (outward)
  const upper = bone('leftUpperArm', shoulder, [0, 0.09, 0]);
  const helper = bone('arm_left_shoulder_2_ctr', upper, [0, 0, 0]);
  const helper2 = bone('arm_left_shoulder_2_ctr_2', helper, [0, 0.12, 0]);
  const lower = bone('leftLowerArm', upper, [0, 0.24, 0]);
  const hand = bone('leftHand', lower, [0, 0.23, 0]);
  const finger = bone('leftIndexProximal', hand, [0, 0.07, 0]);
  const thigh = bone('leftUpperLeg', lowerBody, [0.09, -0.02, 0]);
  thigh.rotation.z = Math.PI; // Y points down
  const calf = bone('leftLowerLeg', thigh, [0, 0.42, 0]);
  const foot = bone('leftFoot', calf, [0, 0.44, 0]);
  const breast = bone('breast_left', chest, [0.045, 0.085, 0.03]);
  model.updateMatrixWorld(true);
  const list = [hips, lowerBody, spine, chest, neck, head, shoulder, upper, helper, helper2, lower, hand, finger, thigh, calf, foot, breast];
  const geo = new BoxGeometry(0.1, 0.1, 0.1);
  const n = geo.attributes.position!.count;
  const idx = new Uint16Array(n * 4);
  const w = new Float32Array(n * 4);
  for (let i = 0; i < n; i++) {
    idx[i * 4] = list.indexOf(upper);
    w[i * 4] = 1;
  }
  geo.setAttribute('skinIndex', new Uint16BufferAttribute(idx, 4));
  geo.setAttribute('skinWeight', new Float32BufferAttribute(w, 4));
  const skin = new SkinnedMesh(geo, new MeshStandardMaterial({ name: 'Dress' }));
  model.add(skin);
  skin.bind(new Skeleton(list), new Matrix4());
  const bones = Object.fromEntries(list.map((b) => [b.name, b]));
  return { model, bones, skin };
}

/** A face: two eye quads, lashes above them, a skin grid with a `mouth_i` morph. */
function face(): Group {
  const g = new Group();
  const quad = (cx: number, cy: number, w: number, h: number, name: string, z = 0.1) => {
    const geo = new PlaneGeometry(w, h, 2, 2);
    geo.translate(cx, cy, z);
    const m = new Mesh(geo, new MeshStandardMaterial({ name }));
    g.add(m);
    return m;
  };
  quad(1, 0, 0.5, 0.6, 'EyeL');
  quad(-1, 0, 0.5, 0.6, 'EyeR');
  quad(1, 0.75, 0.8, 0.2, 'Lash', 0.11);
  quad(-1, 0.75, 0.8, 0.2, 'Lash', 0.11);
  const skin = quad(0, -0.8, 4, 4, 'FaceSkin', 0.09);
  const geo = skin.geometry as BufferGeometry;
  // finer skin grid
  const fine = new PlaneGeometry(4, 4, 40, 40);
  fine.translate(0, -0.8, 0.09);
  skin.geometry = fine;
  geo.dispose();
  const p = fine.attributes.position!;
  const delta = new Float32Array(p.count * 3);
  for (let i = 0; i < p.count; i++) {
    const x = p.getX(i), y = p.getY(i);
    if (Math.abs(y + 1.6) < 0.15 && Math.abs(x) < 0.6) delta[i * 3] = Math.sign(x) * 0.05;
  }
  fine.morphAttributes.position = [new Float32BufferAttribute(delta, 3)];
  fine.morphTargetsRelative = true;
  skin.updateMorphTargets();
  skin.morphTargetDictionary = { mouth_i: 0 };
  return g;
}

// ---------------------------------------------------------------------------------------------

describe('look values', () => {
  it('maps 0/50/100 to min/1/max, piecewise', () => {
    expect(sliderFactor({ min: 0.75, max: 1.35 }, 50)).toBe(1);
    expect(sliderFactor({ min: 0.75, max: 1.35 }, 0)).toBeCloseTo(0.75);
    expect(sliderFactor({ min: 0.75, max: 1.35 }, 100)).toBeCloseTo(1.35);
    expect(sliderFactor({ min: 0.75, max: 1.35 }, 75)).toBeCloseTo(1.175);
    expect(sliderFactor({ min: -1, max: 1 }, 25)).toBeCloseTo(-0.5);
    // clamped
    expect(sliderFactor({ min: 0.9, max: 1.1 }, 400)).toBeCloseTo(1.1);
    expect(sliderFactor({ min: 0.9, max: 1.1 }, -5)).toBeCloseTo(0.9);
  });

  it('normalizes untrusted input without throwing', () => {
    const l = normalizeLook({
      body: { height: 120, head: 'big', bogus: 3 },
      face: { eyeSize: 33.4 },
      eyes: { iris: '#ABCDEF', highlight: false },
      skin: { tone: 'red' },
      makeup: { lipstick: { color: '#112233', amount: 999 } },
      hair: { style: '../x', color: '#00ff00' },
      accessories: { head: 'beret', tail: 'x', eyes: 'Bad Id' },
      tiara: 0,
      outfit: { id: 'gym', parts: { top: 'casual-top', hat: 'x' } },
    });
    expect(l.body.height).toBe(100);
    expect(l.body.head).toBe(50);
    expect('bogus' in l.body).toBe(false);
    expect(l.face.eyeSize).toBe(33);
    expect(l.eyes).toEqual({ iris: '#abcdef', highlight: false });
    expect(l.skin.tone).toBeNull();
    expect(l.makeup.lipstick).toEqual({ color: '#112233', amount: 100 });
    expect(l.hair).toEqual({ style: null, color: '#00ff00', highlight: null });
    expect(l.accessories).toEqual({ head: 'beret' });
    expect(l.tiara).toBe(true);
    expect(l.outfit).toEqual({ id: 'gym', parts: { top: 'casual-top' } });
    expect(normalizeLook(null)).toEqual(defaultLook());
    expect(normalizeLook('x')).toEqual(defaultLook());
  });

  it('resets one category or everything', () => {
    const l = normalizeLook({ body: { height: 90 }, face: { eyeSize: 10 }, accessories: { head: 'beret' }, tiara: false });
    const b = resetCategory(l, 'body');
    expect(b.body.height).toBe(50);
    expect(b.face.eyeSize).toBe(10);
    const a = resetCategory(l, 'accessories');
    expect(a.accessories).toEqual({});
    expect(a.tiara).toBe(true);
    expect(looksEqual(resetCategory(l, 'looks'), defaultLook())).toBe(true);
  });

  it('randomises within taste, deterministically per seed', () => {
    const opts = { accessories: { head: ['beret', 'crown'], hair: ['ribbon'] }, hairstyles: ['natural', 'ponytail'] };
    const a = randomLook(defaultLook(), rng(42), opts);
    const b = randomLook(defaultLook(), rng(42), opts);
    expect(a).toEqual(b);
    for (let seed = 0; seed < 50; seed++) {
      const r = randomLook(defaultLook(), rng(seed), opts);
      for (const s of BODY_SLIDERS) {
        expect(r.body[s.id]).toBeGreaterThanOrEqual(s.taste[0]);
        expect(r.body[s.id]).toBeLessThanOrEqual(s.taste[1]);
      }
      for (const s of FACE_SLIDERS) {
        expect(r.face[s.id]).toBeGreaterThanOrEqual(s.taste[0]);
        expect(r.face[s.id]).toBeLessThanOrEqual(s.taste[1]);
      }
      if (r.accessories.head) expect(r.tiara).toBe(false);
      expect(normalizeLook(r)).toEqual(r);
    }
    // only the asked categories change
    const onlyBody = randomLook(defaultLook(), rng(1), { categories: ['body'] });
    expect(onlyBody.face).toEqual(defaultLook().face);
    expect(onlyBody.accessories).toEqual({});
  });

  it('undo/redo are bounded stacks', () => {
    const h = new LookHistory(3);
    let cur = defaultLook();
    for (let i = 1; i <= 5; i++) {
      h.record(cur);
      cur = normalizeLook({ body: { height: 50 + i } });
    }
    const seen: number[] = [];
    let prev: Look | null;
    while ((prev = h.undo(cur))) {
      cur = prev;
      seen.push(cur.body.height!);
    }
    expect(seen).toEqual([54, 53, 52]);
    expect(h.canRedo).toBe(true);
    cur = h.redo(cur)!;
    expect(cur.body.height).toBe(53);
    h.record(cur);
    expect(h.canRedo).toBe(false);
  });

  it('converts colours both ways', () => {
    const [h, s, v] = hexToHsv('#3d6fd1');
    expect(hsvToHex(h, s, v)).toBe('#3d6fd1');
    expect(hexToHsv('#ff0000')).toEqual([0, 1, 1]);
  });
});

describe('body deformer', () => {
  it('stretches limbs through the skin and child positions, keeping children unscaled', () => {
    const { model, bones } = rig();
    const d = new BodyDeformer(model);
    expect(d.found).toEqual(expect.arrayContaining(['neck', 'head', 'upperArmL', 'lowerArmL', 'handL', 'upperLegL', 'breastL']));
    d.apply(normalizeLook({ body: { armLength: 100, armThickness: 100 } }));
    expect(bones.leftLowerArm!.position.y).toBeCloseTo(0.24 * 1.1);
    expect(bones.leftHand!.position.y).toBeCloseTo(0.23 * 1.1);
    expect(bones.arm_left_shoulder_2_ctr_2!.position.y).toBeCloseTo(0.12 * 1.1);
    // node scales untouched: children (hand, props) never stretch
    expect(bones.leftUpperArm!.scale.toArray()).toEqual([1, 1, 1]);
    const s = new Vector3().setFromMatrixScale(skinDeformOf(bones.leftUpperArm!)!);
    expect(s.x).toBeCloseTo(1.2);
    expect(s.y).toBeCloseTo(1.1);
    expect(skinDeformOf(bones.arm_left_shoulder_2_ctr!)).toBeDefined();
    // back to default removes the deform and restores the rest positions
    d.apply(defaultLook());
    expect(skinDeformOf(bones.leftUpperArm!)).toBeUndefined();
    expect(bones.leftLowerArm!.position.y).toBeCloseTo(0.24);
  });

  it('applies the deform in the skinning matrix only', () => {
    const { model, bones, skin } = rig();
    const d = new BodyDeformer(model);
    d.apply(normalizeLook({ body: { armThickness: 100 } }));
    model.updateMatrixWorld(true);
    skin.skeleton.update();
    const i = skin.skeleton.bones.indexOf(bones.leftUpperArm!);
    const m = new Matrix4().fromArray(skin.skeleton.boneMatrices!, i * 16);
    // the bone matrix scales by 1.2 across the bone (local X and Z)
    const s = new Vector3().setFromMatrixScale(m);
    expect(Math.max(s.x, s.y, s.z)).toBeCloseTo(1.2, 3);
    // the bone itself did not move
    expect(bones.leftUpperArm!.matrixWorld.elements).toEqual(bones.leftUpperArm!.matrixWorld.elements);
  });

  it('scales head and hands uniformly, height on the model, lifts for longer legs', () => {
    const { model, bones } = rig();
    const d = new BodyDeformer(model);
    d.apply(normalizeLook({ body: { head: 100, handSize: 0, height: 0, legLength: 100 } }));
    expect(bones.head!.scale.x).toBeCloseTo(1.12);
    expect(bones.leftHand!.scale.y).toBeCloseTo(0.85);
    expect(model.scale.x).toBeCloseTo(0.92);
    // legs 0.42 + 0.44 long, 10% longer, model at 0.92
    expect(model.position.y).toBeCloseTo(0.86 * 0.1 * 0.92, 4);
    expect(bones.leftLowerLeg!.position.y).toBeCloseTo(0.462);
  });

  it('clamps out-of-range slider values', () => {
    const { model, bones } = rig();
    const d = new BodyDeformer(model);
    d.apply({ body: { neckLength: 9999 } });
    expect(bones.head!.position.y).toBeCloseTo(0.07 * 1.25);
    d.apply({ body: { neckLength: -50 } });
    expect(bones.head!.position.y).toBeCloseTo(0.07 * 0.8);
  });
});

describe('face morphs', () => {
  it('finds landmarks from the eye primitives and the mouth morph', () => {
    const lm = faceLandmarks(findFaceParts(face()))!;
    expect(lm.u).toBeCloseTo(1);
    expect(lm.eyeL[0]).toBeCloseTo(1);
    expect(lm.mouth[1]).toBeCloseTo(-1.6, 1);
    expect(lm.browL[1]).toBeGreaterThan(0.6);
  });

  it('appends signed targets, keeping the influence arrays', () => {
    const g = face();
    const skin = g.children.find((m) => (m as Mesh).material && ((m as Mesh).material as MeshStandardMaterial).name === 'FaceSkin') as Mesh;
    const influences = skin.morphTargetInfluences!;
    const fm = new FaceMorpher(g);
    expect(fm.ready).toBe(true);
    expect(skin.morphTargetInfluences).toBe(influences);
    expect(skin.morphTargetDictionary!.ed_eyeSize).toBeDefined();
    expect(skin.geometry.morphAttributes.position!.length).toBe(1 + FACE_SLIDERS.length);
    fm.apply(normalizeLook({ face: { eyeSize: 100, mouthWidth: 0 } }));
    expect(influences[skin.morphTargetDictionary!.ed_eyeSize!]).toBe(1);
    expect(influences[skin.morphTargetDictionary!.ed_mouthWidth!]).toBe(-1);
    expect(influences[0]).toBe(0); // the model's own morph is untouched
  });

  it('eye size pushes the eye outline outward and leaves the far skin alone', () => {
    const g = face();
    new FaceMorpher(g);
    const eye = g.children.find((m) => ((m as Mesh).material as MeshStandardMaterial).name === 'EyeL') as Mesh;
    const i = eye.morphTargetDictionary!.ed_eyeSize!;
    const t = eye.geometry.morphAttributes.position![i]!;
    const p = eye.geometry.attributes.position!;
    // corner vertex (1.25, 0.3): moves away from the eye centre (1, 0)
    for (let k = 0; k < p.count; k++) {
      if (Math.abs(p.getX(k) - 1.25) < 1e-6 && Math.abs(p.getY(k) - 0.3) < 1e-6) {
        expect(t.getX(k)).toBeGreaterThan(0);
        expect(t.getY(k)).toBeGreaterThan(0);
      }
    }
    const skin = g.children.find((m) => ((m as Mesh).material as MeshStandardMaterial).name === 'FaceSkin') as Mesh;
    const ts = skin.geometry.morphAttributes.position![skin.morphTargetDictionary!.ed_eyeSize!]!;
    const ps = skin.geometry.attributes.position!;
    for (let k = 0; k < ps.count; k++) if (ps.getY(k) < -2) expect(ts.getX(k)).toBe(0);
  });

  it('draws makeup masks around the landmarks', () => {
    const g = face();
    const parts = findFaceParts(g);
    const lm = faceLandmarks(parts)!;
    const [lip] = makeupAt([lm.mouth[0], lm.mouth[1], lm.mouth[2] + 0.01], lm);
    expect(lip).toBeGreaterThan(0.9);
    expect(makeupAt([3, 3, 0.1], lm)).toEqual([0, 0, 0, 0]);
    // behind the head: nothing
    expect(makeupAt([lm.mouth[0], lm.mouth[1], -5], lm)).toEqual([0, 0, 0, 0]);
    const tex = makeupMask(parts, lm, { size: 64 });
    const data = tex.image.data as Uint8Array;
    let red = 0;
    for (let i = 0; i < data.length; i += 4) red = Math.max(red, data[i]!);
    expect(red).toBeGreaterThan(200);
  });
});

describe('accessories', () => {
  it('parses an untrusted list', () => {
    const list = parseAccessories([
      { id: 'beret', slot: 'head', glb: 'acc-beret.glb', name: { it: 'Basco', fr: 'x' }, hideTiara: true },
      { id: 'beret', slot: 'head', glb: 'acc-beret.glb' },
      { id: 'bad', slot: 'tail', glb: 'acc-x.glb' },
      { id: 'evil', slot: 'head', glb: '../etc/passwd.glb' },
      { id: 'pic', slot: 'eyes', glb: 'acc-g.glb', thumbnail: 'acc-g.png', bone: 'head' },
    ]);
    expect(list.map((a) => a.id)).toEqual(['beret', 'pic']);
    expect(list[0]!.name).toEqual({ it: 'Basco' });
    expect(list[0]!.hideTiara).toBe(true);
  });

  it('attaches a rigid prop where it was modelled, follows the bone, and detaches', () => {
    const { model, bones } = rig();
    const tiara = new Mesh(new BoxGeometry(), new MeshStandardMaterial({ name: 'Tiara' }));
    model.add(tiara);
    const acc = new AccessoryRig(model);
    const prop = new Group();
    const m = new Mesh(new BoxGeometry(0.01, 0.01, 0.01), new MeshStandardMaterial({ name: 'Acc' }));
    m.position.set(0.05, 1.5, 0.08); // modelled in rest space
    prop.add(m);
    expect(acc.attach({ id: 'beret', slot: 'head', glb: 'acc-beret.glb', hideTiara: true }, prop)).toBe(true);
    model.updateMatrixWorld(true);
    const at = m.getWorldPosition(new Vector3());
    expect(at.x).toBeCloseTo(0.05);
    expect(at.y).toBeCloseTo(1.5);
    expect(at.z).toBeCloseTo(0.08);
    expect(tiara.visible).toBe(false);
    // turn the head: the prop follows
    bones.head!.quaternion.setFromAxisAngle(new Vector3(0, 1, 0), Math.PI / 2);
    model.updateMatrixWorld(true);
    expect(m.getWorldPosition(new Vector3()).x).not.toBeCloseTo(0.05);
    expect(acc.slots).toEqual({ head: 'beret' });
    acc.detach('head');
    expect(m.parent?.parent?.parent?.parent ?? null).toBeNull();
    expect(tiara.visible).toBe(true);
    acc.setTiara(false);
    expect(tiara.visible).toBe(false);
  });

  it('applyLook loads the props of a look and drops stale ones', async () => {
    const { model } = rig();
    const loads: string[] = [];
    const loader = vi.fn(async (url: string) => {
      loads.push(url);
      const g = new Group();
      g.add(new Mesh(new BoxGeometry(0.01, 0.01, 0.01), new MeshStandardMaterial({ name: 'Acc' })));
      return g;
    });
    const catalog = { accessories: [{ id: 'ribbon', slot: 'hair', glb: 'acc-ribbon.glb' }, { id: 'band', slot: 'wristL', glb: 'acc-band.glb' }] };
    await applyLook(model, normalizeLook({ accessories: { hair: 'ribbon', wristL: 'band' } }), { loader, catalog, assetBase: '/a/' });
    expect(loads.sort()).toEqual(['/a/acc-band.glb', '/a/acc-ribbon.glb']);
    const names: string[] = [];
    model.traverse((o) => o.name.startsWith('Acc:') && names.push(o.name));
    expect(names.sort()).toEqual(['Acc:hair:ribbon', 'Acc:wristL:band']);
    // the same look again loads nothing; dropping the ribbon detaches it
    await applyLook(model, normalizeLook({ accessories: { wristL: 'band' } }), { loader, catalog, assetBase: '/a/' });
    expect(loads.length).toBe(2);
    names.length = 0;
    model.traverse((o) => o.name.startsWith('Acc:') && names.push(o.name));
    expect(names).toEqual(['Acc:wristL:band']);
  });
});

describe('outfit parts', () => {
  const parts = parseParts([
    { id: 'casual-top', outfit: 'casual', slot: 'top', glb: 'lucina-casual-top.glb', hides: ['Body_chest', 'Body_belly', 'nope'] },
    { id: 'casual-bottom', outfit: 'casual', slot: 'bottom', glb: 'lucina-casual-bottom.glb', hides: ['Body_hips'] },
    { id: 'maid-onepiece', outfit: 'maid', slot: 'onepiece', glb: 'lucina-maid-onepiece.glb', hides: ['Body_chest', 'Body_hips'] },
    { id: 'bad', outfit: 'x', slot: 'hat', glb: 'x.glb' },
  ]);

  it('parses and validates', () => {
    expect(parts.map((p) => p.id)).toEqual(['casual-top', 'casual-bottom', 'maid-onepiece']);
    expect(parts[0]!.hides).toEqual(['Body_chest', 'Body_belly']);
    expect(parts[2]!.occupies).toEqual(['top', 'bottom']);
  });

  it('resolves occupancy: the last pick wins', () => {
    expect(resolveParts({ top: 'casual-top', bottom: 'casual-bottom' }, parts)).toEqual({ top: 'casual-top', bottom: 'casual-bottom' });
    expect(resolveParts({ top: 'casual-top', onepiece: 'maid-onepiece' }, parts, 'onepiece')).toEqual({ onepiece: 'maid-onepiece' });
    expect(resolveParts({ top: 'casual-top', onepiece: 'maid-onepiece' }, parts, 'top')).toEqual({ top: 'casual-top' });
    expect(resolveParts({ top: 'maid-onepiece' }, parts)).toEqual({});
  });

  it('re-binds a part to the base, hides covered regions, grafts missing bones', () => {
    const { model, bones } = rig();
    for (const r of ['Body_chest', 'Body_belly', 'Body_hips']) {
      const m = new Mesh(new BoxGeometry(), new MeshStandardMaterial({ name: 'SkinTinted' }));
      m.name = r;
      model.add(m);
    }
    const pr = new PartsRig(model);
    expect(pr.regionNames.sort()).toEqual(['Body_belly', 'Body_chest', 'Body_hips']);
    // a part with its own copy of the chest bone plus a skirt bone the base lacks
    const partRoot = new Group();
    const pChest = bone('chest', partRoot, [0, 1.2, 0]);
    const skirt = bone('skirt0_0', pChest, [0, -0.2, 0.1]);
    const geo = new BoxGeometry();
    const n = geo.attributes.position!.count;
    geo.setAttribute('skinIndex', new Uint16BufferAttribute(new Uint16Array(n * 4), 4));
    const w = new Float32Array(n * 4);
    for (let i = 0; i < n; i++) w[i * 4] = 1;
    geo.setAttribute('skinWeight', new Float32BufferAttribute(w, 4));
    const sm = new SkinnedMesh(geo, new MeshStandardMaterial({ name: 'Clothes_Casual_Top' }));
    partRoot.add(sm);
    partRoot.updateMatrixWorld(true);
    sm.bind(new Skeleton([pChest, skirt]), new Matrix4());
    expect(pr.attach(parts[0]!, partRoot)).toBe(true);
    expect(sm.skeleton.bones[0]).toBe(bones.chest);
    expect(sm.skeleton.bones[1]!.name).toBe('skirt0_0');
    expect(sm.skeleton.bones[1]!.parent).toBe(bones.chest);
    const vis = (n: string) => (model.getObjectByName(n) as Mesh).visible;
    expect(vis('Body_chest')).toBe(false);
    expect(vis('Body_belly')).toBe(false);
    expect(vis('Body_hips')).toBe(true);
    pr.detach('top');
    expect(vis('Body_chest')).toBe(true);
  });
});

describe('editing session', () => {
  function fakeStage(): { stage: LucinaStage; model: Group; refit: ReturnType<typeof vi.fn>; setView: ReturnType<typeof vi.fn> } {
    const { model } = rig();
    const refit = vi.fn();
    const setView = vi.fn();
    const stage = { three: { model }, refit, setView, cameraPreset: vi.fn() } as unknown as LucinaStage;
    return { stage, model, refit, setView };
  }

  it('applies live, commits once per gesture, undoes and redoes', async () => {
    const { stage, model, refit } = fakeStage();
    const ed = createAvatarEditor(stage);
    const changes: boolean[] = [];
    ed.onChange((_l, c) => changes.push(c));
    ed.update((l) => (l.body.height = 60));
    ed.update((l) => (l.body.height = 70));
    expect(model.scale.x).toBeCloseTo(1 + 0.08 * 0.4);
    expect(refit).toHaveBeenCalled();
    ed.commit();
    expect(changes).toEqual([false, false, true]);
    ed.update((l) => (l.face.eyeSize = 80));
    ed.commit();
    expect(ed.undo()).toBe(true);
    expect(ed.look.face.eyeSize).toBe(50);
    expect(ed.look.body.height).toBe(70);
    expect(ed.undo()).toBe(true);
    expect(ed.look.body.height).toBe(50);
    expect(model.scale.x).toBeCloseTo(1);
    expect(ed.undo()).toBe(false);
    expect(ed.redo()).toBe(true);
    expect(ed.look.body.height).toBe(70);
    ed.reset('body');
    expect(ed.look.body.height).toBe(50);
    expect(ed.canUndo).toBe(true);
    ed.randomize({ seed: 7, categories: ['body'] });
    expect(ed.look.body).not.toEqual(defaultLook().body);
    ed.focus('face');
    ed.dispose();
    await ed.settled();
  });

  it('prepareModel dresses a freshly loaded model in the current look', async () => {
    const { stage } = fakeStage();
    const ed = createAvatarEditor(stage, { look: normalizeLook({ body: { head: 100 } }) });
    const { model, bones } = rig();
    await ed.prepareModel(model);
    expect(bones.head!.scale.x).toBeCloseTo(1.12);
    ed.dispose();
  });

  it('keeps quaternions of bones (clips own rotations)', () => {
    const { model, bones } = rig();
    const q = new Quaternion().setFromAxisAngle(new Vector3(1, 0, 0), 0.4);
    bones.leftUpperArm!.quaternion.copy(q);
    new BodyDeformer(model).apply(normalizeLook({ body: { armLength: 0, shoulders: 100 } }));
    expect(bones.leftUpperArm!.quaternion.equals(q)).toBe(true);
  });
});
