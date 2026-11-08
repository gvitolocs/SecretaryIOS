// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest';
import { Bone, BoxGeometry, Group, Mesh, MeshBasicMaterial, MeshStandardMaterial, PerspectiveCamera, Quaternion, Ray, Scene, Skeleton, SkinnedMesh, Vector3 } from 'three';
import {
  applyDance,
  DanceLayer,
  applyFilterPixels,
  applyHand,
  attachStudioExtras,
  bindHand,
  cleanPose,
  createStageHooks,
  cssFilter,
  DANCE_MOVES,
  defaultSlot,
  FaceMixer,
  findHumanoid,
  flickSlot,
  HAND_PRESETS,
  History,
  listMorphs,
  MAX_PROPS,
  normName,
  parseSettings,
  PoseEditor,
  PoseLayer,
  PropLibrary,
  PropSet,
  restPose,
  sampleDance,
  solveTwoBone,
  STAMP_IDS,
  STAMPS,
  vignetteCss,
  withRestPose,
  worldPos,
  type FrameContext,
} from '../src/studio2';
import type { LucinaStage } from '../src/stage';

/** World positions (metres) of a G-like rig, measured from Lucina G's rest pose. */
const G: [string, string | null, [number, number, number]][] = [
  ['root', null, [0, 0, 0]],
  ['hips', 'root', [0, 0.981, 0]],
  ['lowerBody', 'hips', [0, 0.981, 0]],
  ['spine', 'hips', [0, 1.05, 0]],
  ['chest', 'spine', [0, 1.2, 0]],
  ['neck', 'chest', [0, 1.388, -0.02]],
  ['head', 'neck', [0, 1.455, -0.02]],
  ['leftShoulder', 'chest', [0.04, 1.34, -0.03]],
  ['leftUpperArm', 'leftShoulder', [0.106, 1.353, -0.034]],
  ['leftLowerArm', 'leftUpperArm', [0.219, 1.141, -0.041]],
  ['leftHand', 'leftLowerArm', [0.327, 0.943, 0.001]],
  ['leftThumbProximal', 'leftHand', [0.323, 0.935, 0.02]],
  ['leftThumbIntermediate', 'leftThumbProximal', [0.326, 0.899, 0.061]],
  ['leftThumbDistal', 'leftThumbIntermediate', [0.327, 0.882, 0.082]],
  ['leftIndexProximal', 'leftHand', [0.367, 0.889, 0.038]],
  ['leftIndexIntermediate', 'leftIndexProximal', [0.384, 0.857, 0.044]],
  ['leftIndexDistal', 'leftIndexIntermediate', [0.395, 0.837, 0.048]],
  ['leftMiddleProximal', 'leftHand', [0.369, 0.887, 0.021]],
  ['leftMiddleIntermediate', 'leftMiddleProximal', [0.387, 0.852, 0.022]],
  ['leftMiddleDistal', 'leftMiddleIntermediate', [0.398, 0.83, 0.023]],
  ['leftRingProximal', 'leftHand', [0.364, 0.887, 0.003]],
  ['leftRingIntermediate', 'leftRingProximal', [0.381, 0.854, 0.002]],
  ['leftRingDistal', 'leftRingIntermediate', [0.391, 0.834, 0.001]],
  ['leftLittleProximal', 'leftHand', [0.356, 0.887, -0.015]],
  ['leftLittleIntermediate', 'leftLittleProximal', [0.369, 0.861, -0.017]],
  ['leftLittleDistal', 'leftLittleIntermediate', [0.377, 0.845, -0.018]],
  ['leftUpperLeg', 'lowerBody', [0.086, 0.956, 0]],
  ['leftLowerLeg', 'leftUpperLeg', [0.086, 0.536, 0.015]],
  ['leftFoot', 'leftLowerLeg', [0.086, 0.097, 0]],
  ['leftToes', 'leftFoot', [0.086, 0.026, 0.09]],
];

function mirror(): typeof G {
  return G.filter(([n]) => n.startsWith('left')).map(([n, p, [x, y, z]]) => [n.replace('left', 'right'), p && p.startsWith('left') ? p.replace('left', 'right') : p, [-x, y, z]]);
}

/** A skinned G-like model: bones with identity rotations, a face mesh with a few morphs. */
function makeModel(): { model: Group; bones: Map<string, Bone> } {
  const model = new Group();
  model.name = 'Lucina';
  const bones = new Map<string, Bone>();
  const world = new Map<string, Vector3>();
  for (const [name, parent, p] of [...G, ...mirror()]) {
    const b = new Bone();
    b.name = name;
    const w = new Vector3(...p);
    world.set(name, w);
    if (parent) {
      b.position.copy(w.clone().sub(world.get(parent)!));
      bones.get(parent)!.add(b);
    } else model.add(b);
    bones.set(name, b);
  }
  model.updateMatrixWorld(true);
  const geo = new BoxGeometry(0.1, 0.1, 0.1);
  geo.morphAttributes.position = [geo.attributes.position!.clone(), geo.attributes.position!.clone(), geo.attributes.position!.clone()];
  const face = new SkinnedMesh(geo, new MeshBasicMaterial());
  face.name = 'Lucina_Face';
  face.morphTargetDictionary = { blink: 0, smile: 1, 'ファルシオンOFF': 2 };
  face.morphTargetInfluences = [0, 0, 0];
  model.add(face);
  face.bind(new Skeleton([...bones.values()]));
  return { model, bones };
}

describe('humanoid bones', () => {
  it('finds the G/VRM names, fingers and legs', () => {
    const { model } = makeModel();
    const h = findHumanoid(model);
    expect(h.hips?.name).toBe('hips');
    expect(h.head?.name).toBe('head');
    expect(h.l.upperArm?.name).toBe('leftUpperArm');
    expect(h.r.lowerLeg?.name).toBe('rightLowerLeg');
    expect(h.l.foot?.name).toBe('leftFoot');
    expect(h.l.toes?.name).toBe('leftToes');
    expect(h.fingers.l.index?.map((b) => b.name)).toEqual(['leftIndexProximal', 'leftIndexIntermediate', 'leftIndexDistal']);
    expect(h.fingers.r.thumb?.length).toBe(3);
  });

  it('reads Mixamo, VRoid, Rigify and MMD names', () => {
    const root = new Group();
    for (const n of ['mixamorig:LeftArm', 'mixamorig:LeftForeArm', 'mixamorig:LeftUpLeg', 'mixamorig:LeftLeg', 'mixamorig:LeftHandIndex1', 'J_Bip_R_UpperArm', 'DEF-thigh.R', '右ひじ', '右人指１', '右人指２']) {
      const b = new Bone();
      b.name = n;
      root.add(b);
    }
    const h = findHumanoid(root);
    expect(h.l.upperArm?.name).toBe('mixamorig:LeftArm');
    expect(h.l.lowerArm?.name).toBe('mixamorig:LeftForeArm');
    expect(h.l.upperLeg?.name).toBe('mixamorig:LeftUpLeg');
    expect(h.l.lowerLeg?.name).toBe('mixamorig:LeftLeg');
    expect(h.fingers.l.index?.[0]?.name).toBe('mixamorig:LeftHandIndex1');
    expect(h.r.upperArm?.name).toBe('J_Bip_R_UpperArm');
    expect(h.r.upperLeg?.name).toBe('DEF-thigh.R');
    expect(h.r.lowerArm?.name).toBe('右ひじ');
    expect(h.fingers.r.index?.map((b) => b.name)).toEqual(['右人指１', '右人指２']);
    expect(normName('mixamorig:LeftHand')).toBe('lefthand');
  });
});

describe('two-bone IK', () => {
  it('reaches a reachable target and keeps the hand orientation', () => {
    const { model, bones } = makeModel();
    const [a, b, c] = ['leftUpperArm', 'leftLowerArm', 'leftHand'].map((n) => bones.get(n)!);
    const handRot = c!.getWorldQuaternion(new Quaternion());
    const target = new Vector3(0.3, 1.25, 0.2);
    const miss = solveTwoBone(a!, b!, c!, target, new Vector3(0.3, -0.2, -1).normalize());
    model.updateMatrixWorld(true);
    expect(miss).toBeLessThan(1e-3);
    expect(worldPos(c!).distanceTo(target)).toBeLessThan(1e-3);
    expect(c!.getWorldQuaternion(new Quaternion()).angleTo(handRot)).toBeLessThan(1e-4);
    // Bone lengths are unchanged (rotations only).
    expect(worldPos(b!).distanceTo(worldPos(a!))).toBeCloseTo(new Vector3(0.113, -0.212, -0.007).length(), 5);
  });

  it('bends a straight leg toward the pole (knee forward)', () => {
    const { bones } = makeModel();
    const [a, b, c] = ['leftUpperLeg', 'leftLowerLeg', 'leftFoot'].map((n) => bones.get(n)!);
    // Lift the foot 25 cm: the knee must come forward (+Z).
    const t = worldPos(c!).clone().add(new Vector3(0, 0.25, 0));
    solveTwoBone(a!, b!, c!, t, new Vector3(0, 0, 1));
    expect(worldPos(c!).distanceTo(t)).toBeLessThan(1e-3);
    expect(worldPos(b!).z).toBeGreaterThan(0.05);
  });

  it('stretches toward an unreachable target', () => {
    const { bones } = makeModel();
    const [a, b, c] = ['leftUpperArm', 'leftLowerArm', 'leftHand'].map((n) => bones.get(n)!);
    const t = new Vector3(2, 1.35, 0);
    const miss = solveTwoBone(a!, b!, c!, t, new Vector3(0, 0, -1));
    expect(miss).toBeGreaterThan(1);
    const dir = worldPos(c!).clone().sub(worldPos(a!)).normalize();
    expect(dir.dot(t.clone().sub(worldPos(a!)).normalize())).toBeGreaterThan(0.999);
  });
});

describe('hand shapes', () => {
  it('a fist curls the fingertips toward the palm; open spreads them', () => {
    const { model, bones } = makeModel();
    const h = findHumanoid(model);
    const rest = restPose(model);
    const hb = withRestPose(model, rest, () => bindHand(h, 'l', rest.q))!;
    expect(hb).toBeTruthy();
    const tip = bones.get('leftIndexDistal')!;
    const hand = bones.get('leftHand')!;
    const before = worldPos(tip).clone().sub(worldPos(hand));
    applyHand(hb, HAND_PRESETS.fist, 1);
    model.updateMatrixWorld(true);
    const after = worldPos(tip).clone().sub(worldPos(hand));
    // The palm faces her body (-X for the left hand): a fist pulls the tip inward and shortens the reach.
    expect(after.x).toBeLessThan(before.x - 0.01);
    expect(after.length()).toBeLessThan(before.length() - 0.01);
    applyHand(hb, HAND_PRESETS.open, 1);
    model.updateMatrixWorld(true);
    const idx = worldPos(bones.get('leftIndexDistal')!).clone();
    const lit = worldPos(bones.get('leftLittleDistal')!).clone();
    expect(idx.distanceTo(lit)).toBeGreaterThan(before.clone().add(worldPos(hand)).distanceTo(worldPos(bones.get('leftLittleDistal')!)) - 0.05);
  });

  it('rest pose comes from the inverse bind matrices, not the current pose', () => {
    const { model, bones } = makeModel();
    const b = bones.get('leftIndexProximal')!;
    const restQ = b.quaternion.clone();
    b.quaternion.setFromAxisAngle(new Vector3(1, 0, 0), 1);
    const rest = restPose(model);
    expect(rest.q.get(b)!.angleTo(restQ)).toBeLessThan(1e-5);
  });
});

describe('poses', () => {
  it('the pose layer holds a pose over the clips and clears back', () => {
    const { model, bones } = makeModel();
    const layer = new PoseLayer();
    layer.bind(model);
    const q = new Quaternion().setFromAxisAngle(new Vector3(0, 0, 1), 0.7);
    layer.set({ name: 'x', bones: { leftUpperArm: [q.x, q.y, q.z, q.w] }, positions: { hips: [0, 0.9, 0] } }, false);
    layer.apply(1);
    expect(bones.get('leftUpperArm')!.quaternion.angleTo(q)).toBeLessThan(1e-6);
    expect(bones.get('hips')!.position.y).toBeCloseTo(0.9);
    layer.clear();
    expect(bones.get('leftUpperArm')!.quaternion.angleTo(new Quaternion())).toBeLessThan(1e-6);
    expect(bones.get('hips')!.position.y).toBeCloseTo(0.981);
  });

  it('dragging the hand handle moves the hand; the pelvis keeps the feet planted', () => {
    const { model, bones } = makeModel();
    const h = findHumanoid(model);
    const cam = new PerspectiveCamera(30, 1, 0.01, 100);
    cam.position.set(0, 1.1, 3);
    cam.lookAt(0, 1.1, 0);
    cam.updateMatrixWorld(true);
    const ed = new PoseEditor();
    ed.bind(h, 1.65, 1);
    const mk = (p: Vector3) => new Ray(cam.position.clone(), p.clone().sub(cam.position).normalize());
    const hand0 = ed.handlePos('hand_l')!.clone();
    expect(ed.begin('hand_l', mk(hand0), cam)).toBe(true);
    const goal = hand0.clone().add(new Vector3(0, 0.25, 0));
    ed.move(mk(goal));
    ed.end();
    model.updateMatrixWorld(true);
    expect(worldPos(bones.get('leftHand')!).distanceTo(goal)).toBeLessThan(0.02);

    const footL = worldPos(bones.get('leftFoot')!).clone();
    const pel0 = ed.handlePos('pelvis')!.clone();
    ed.begin('pelvis', mk(pel0), cam);
    ed.move(mk(pel0.clone().add(new Vector3(0, -0.12, 0))));
    ed.end();
    model.updateMatrixWorld(true);
    expect(worldPos(bones.get('hips')!).y).toBeLessThan(0.9);
    expect(worldPos(bones.get('leftFoot')!).distanceTo(footL)).toBeLessThan(0.005);
    const pose = ed.capture('crouch');
    expect(pose.bones.leftUpperArm).toBeTruthy();
    expect(pose.bones.head).toBeUndefined();
    expect(pose.positions?.hips?.[1]).toBeLessThan(0.9);
    expect(cleanPose(JSON.parse(JSON.stringify(pose)))).toEqual(pose);
  });
});

describe('face mixer', () => {
  it('lists expression morphs without switches, sets untouched morphs and adds on driven ones', () => {
    const { model } = makeModel();
    expect(listMorphs(model)).toEqual(['blink', 'smile']);
    const face = model.getObjectByName('Lucina_Face') as Mesh;
    const inf = face.morphTargetInfluences!;
    const mix = new FaceMixer();
    mix.bind(model);
    mix.weights = { smile: 0.4, blink: 0.3 };
    mix.apply();
    expect(inf[1]).toBeCloseTo(0.4);
    mix.apply(); // no accumulation on a morph nobody else drives
    expect(inf[1]).toBeCloseTo(0.4);
    inf[0] = 0.5; // the rig drove the blink this frame
    mix.apply();
    expect(inf[0]).toBeCloseTo(0.8);
    mix.weights = {};
    mix.apply();
    expect(inf[1]).toBe(0);
  });
});

describe('props', () => {
  function lib(): PropLibrary {
    const l = new PropLibrary();
    const src = new Group();
    for (const k of ['chair', 'cup']) {
      const m = new Mesh(new BoxGeometry(0.4, 0.8, 0.4), new MeshStandardMaterial({ color: 0xff0000 }));
      m.name = k;
      src.add(m);
    }
    l.addTemplates(src);
    return l;
  }

  it('adds, updates (clamped), removes, respects the limit and restores', () => {
    const set = new PropSet(lib());
    const changes = vi.fn();
    set.onChange = changes;
    const a = set.add('chair')!;
    expect(a.kind).toBe('chair');
    expect(set.selected).toBe(a.id);
    expect(set.add('sofa')).toBeNull();
    set.update(a.id, { scale: 99, rot: -90, y: -1 });
    expect(set.get(a.id)).toMatchObject({ scale: 3, rot: 270, y: 0 });
    for (let i = 1; i < MAX_PROPS; i++) expect(set.add('cup')).not.toBeNull();
    expect(set.add('cup')).toBeNull();
    const saved = set.list;
    set.clear();
    expect(set.count).toBe(0);
    set.load(saved);
    expect(set.count).toBe(MAX_PROPS);
    expect(changes).toHaveBeenCalled();
    // Toon materials and an outline per mesh.
    const obj = set.group.children.find((c) => c.userData.propId === a.id)!;
    const mesh = obj as Mesh;
    expect((mesh.material as { type: string }).type).toBe('MeshToonMaterial');
    expect(mesh.children.some((c) => c.userData.outline)).toBe(true);
  });

  it('places new props in a ring of slots around her', () => {
    const s0 = defaultSlot(0, 1.65);
    const s1 = defaultSlot(1, 1.65);
    expect(s0.x).toBeGreaterThan(0);
    expect(s1.x).toBeLessThan(0);
    expect(defaultSlot(12, 1.65)).toEqual(s0);
  });
});

describe('stamps, flick ring, filters, history', () => {
  it('has original SVG stamps', () => {
    expect(STAMP_IDS.length).toBeGreaterThanOrEqual(12);
    for (const id of STAMP_IDS) {
      expect(STAMPS[id]).toMatch(/^<svg [^>]*viewBox="0 0 100 100"/);
      // Well-formed XML (a duplicate attribute makes the browser drop the whole image).
      const doc = new DOMParser().parseFromString(STAMPS[id]!, 'image/svg+xml');
      expect(doc.getElementsByTagName('parsererror').length, id).toBe(0);
    }
  });

  it('maps flick directions clockwise from the top', () => {
    expect(flickSlot(0, -80, 8)).toBe(0);
    expect(flickSlot(80, 0, 8)).toBe(2);
    expect(flickSlot(0, 80, 8)).toBe(4);
    expect(flickSlot(-80, 0, 8)).toBe(6);
    expect(flickSlot(60, -60, 8)).toBe(1);
    expect(flickSlot(5, 5, 8)).toBe(-1);
  });

  it('sepia and mono follow the CSS filter maths; the vignette darkens the corners only', () => {
    const px = () => new Uint8ClampedArray([200, 100, 50, 255]);
    const a = px();
    applyFilterPixels(a, 1, 1, { id: 'mono', strength: 1 });
    expect(a[0]).toBe(a[1]);
    expect(a[1]).toBe(a[2]);
    const b = px();
    applyFilterPixels(b, 1, 1, { id: 'sepia', strength: 0 });
    expect([...b]).toEqual([200, 100, 50, 255]);
    const w = 21;
    const img = new Uint8ClampedArray(w * w * 4).fill(255);
    applyFilterPixels(img, w, w, { id: 'vignette', strength: 1 });
    const at = (x: number, y: number) => img[(y * w + x) * 4]!;
    expect(at(10, 10)).toBe(255);
    expect(at(0, 0)).toBeLessThan(120);
    expect(cssFilter({ id: 'sepia', strength: 0.5 })).toBe('sepia(0.500)');
    expect(vignetteCss({ id: 'none', strength: 1 })).toBe('');
  });

  it('undo and redo walk the snapshots', () => {
    const h = new History({ n: 0 });
    h.push({ n: 1 });
    h.push({ n: 2 });
    expect(h.undo()).toEqual({ n: 1 });
    expect(h.undo()).toEqual({ n: 0 });
    expect(h.canUndo).toBe(false);
    expect(h.redo()).toEqual({ n: 1 });
    h.push({ n: 5 });
    expect(h.canRedo).toBe(false);
  });
});

describe('dance moves', () => {
  it('samples every move and keeps her feet planted', () => {
    const { model, bones } = makeModel();
    const h = findHumanoid(model);
    const feet = ['leftFoot', 'rightFoot'].map((n) => worldPos(bones.get(n)!).clone());
    for (const m of DANCE_MOVES) {
      for (const t of [0.13, 0.61, 1.37]) {
        const f = sampleDance(m, t);
        expect(Number.isFinite(f.hips.y)).toBe(true);
        if (f.lift.l || f.lift.r) continue;
        applyDance(h, f, 1, 1.65, 1);
        model.updateMatrixWorld(true);
        ['leftFoot', 'rightFoot'].forEach((n, i) => expect(worldPos(bones.get(n)!).distanceTo(feet[i]!)).toBeLessThan(0.01));
        withRestPose(model, restPose(model), () => {});
        for (const b of bones.values()) b.quaternion.identity();
        bones.get('hips')!.position.set(0, 0.981, 0);
        model.updateMatrixWorld(true);
      }
    }
  });
});

describe('dance layer', () => {
  it('does not drift when nothing rewrites the bones between frames (no clips)', () => {
    const { model, bones } = makeModel();
    const h = findHumanoid(model);
    const layer = new DanceLayer();
    const foot = worldPos(bones.get('leftFoot')!).clone();
    const hips0 = bones.get('hips')!.position.clone();
    let first: Vector3 | null = null;
    for (let i = 0; i < 120; i++) {
      layer.apply(h, sampleDance('twist', 0.37), 1, 1.65, 1);
      model.updateMatrixWorld(true);
      first ??= bones.get('hips')!.position.clone();
      expect(bones.get('hips')!.position.distanceTo(first)).toBeLessThan(1e-6);
      expect(worldPos(bones.get('leftFoot')!).distanceTo(foot)).toBeLessThan(0.01);
    }
    layer.apply(h, null, 0, 1.65, 1);
    expect(bones.get('hips')!.position.distanceTo(hips0)).toBeLessThan(1e-9);
    expect(bones.get('leftUpperLeg')!.quaternion.angleTo(new Quaternion())).toBeLessThan(1e-6);
  });
});

describe('settings', () => {
  it('sanitises stored settings', () => {
    const s = parseSettings({ hands: { l: 'fist', r: 'claw' }, roll: 400, filter: { id: 'mono', strength: 5 }, flick: 'hold', gaze: { mode: 'custom', x: 3 }, props: [{ id: 'a', kind: 'cup', x: 1, z: 0, y: 0, rot: 10, scale: 9 }], idlePose: 'missing', danceMove: 'twist' });
    expect(s.hands).toEqual({ l: 'fist', r: null });
    expect(s.roll).toBe(45);
    expect(s.filter).toEqual({ id: 'mono', strength: 1 });
    expect(s.gaze).toEqual({ mode: 'custom', x: 1, y: 0 });
    expect(s.props[0]!.scale).toBe(3);
    expect(s.idlePose).toBeNull();
    expect(s.danceMove).toBe('twist');
    expect(parseSettings('garbage').flick).toBe('off');
  });
});

describe('attachStudioExtras', () => {
  function fakeStage() {
    const { model, bones } = makeModel();
    const scene = new Scene();
    scene.add(model);
    const camera = new PerspectiveCamera(30, 1, 0.01, 100);
    camera.position.set(0, 1.2, 3);
    camera.lookAt(0, 1.2, 0);
    const canvas = document.createElement('canvas');
    document.body.append(canvas);
    const hooks = createStageHooks();
    let interactive = true;
    let background = 'default';
    const stage = {
      hooks,
      three: { scene, camera, model, renderer: { domElement: canvas } },
      get interactive() {
        return interactive;
      },
      setInteractive: (on: boolean) => (interactive = on),
      get background() {
        return background;
      },
      setBackground: (id: string) => (background = id),
      info: { clips: [] },
      setExpression: vi.fn(),
      gesture: vi.fn(() => true),
      play: vi.fn(() => Promise.resolve()),
      snapshot: vi.fn(() => Promise.resolve(new Blob(['x'], { type: 'image/png' }))),
      dancing: false,
      clip: null,
      stopClip: vi.fn(),
    } as unknown as LucinaStage;
    const ctx: FrameContext = { dt: 1 / 60, scene, camera, model, renderer: {} as never, canvas, bones: {}, height: 1.65, floorY: 0, reducedMotion: false, dancing: false, placeholder: false };
    const frame = () => {
      for (const f of hooks.pose) f(ctx);
      for (const f of hooks.frame) f(ctx);
      model.updateMatrixWorld(true);
    };
    return { stage, ctx, frame, bones, model, camera };
  }

  it('applies hands, gaze, roll and filter in the frame hook and stores the settings', async () => {
    vi.useFakeTimers();
    const store = new Map<string, string>();
    const { stage, ctx, frame, bones, camera } = fakeStage();
    const ex = attachStudioExtras(stage, { storage: { get: (k) => store.get(k) ?? null, set: (k, v) => void store.set(k, v) } });
    const tip = bones.get('leftIndexDistal')!;
    frame();
    const before = worldPos(tip).clone();
    ex.setHand('l', 'fist');
    for (let i = 0; i < 30; i++) frame();
    expect(worldPos(tip).distanceTo(before)).toBeGreaterThan(0.01);
    ex.setGaze({ mode: 'ahead' });
    expect(stage.hooks.gaze).toEqual({ yaw: 0, pitch: 0 });
    ex.setGaze({ mode: 'camera' });
    expect(stage.hooks.gaze).toBeNull();
    const q0 = camera.quaternion.clone();
    ex.setRoll(20);
    frame();
    expect(camera.quaternion.angleTo(q0)).toBeCloseTo((20 * Math.PI) / 180, 3);
    ex.setFilter({ id: 'sepia', strength: 0.5 });
    frame();
    expect(ctx.canvas.style.filter).toBe('sepia(0.500)');
    vi.advanceTimersByTime(500);
    const saved = JSON.parse(store.get('secretary.studio2')!);
    expect(saved.hands.l).toBe('fist');
    expect(saved.roll).toBe(20);
    ex.setHand('l', null);
    frame();
    expect(worldPos(tip).distanceTo(before)).toBeLessThan(1e-4);
    ex.dispose();
    expect(stage.hooks.frame.size + stage.hooks.pose.size).toBe(0);
    vi.useRealTimers();
  });

  it('edits, saves and holds a pose as her idle; faces save and apply', () => {
    const { stage, frame, bones } = fakeStage();
    const ex = attachStudioExtras(stage, { storage: null });
    frame();
    ex.editPose(true);
    expect(ex.editing).toBe(true);
    // A clip moving the arm is overridden while the pose is held.
    bones.get('leftUpperArm')!.quaternion.setFromAxisAngle(new Vector3(0, 0, 1), 0.5);
    frame();
    expect(bones.get('leftUpperArm')!.quaternion.angleTo(new Quaternion())).toBeLessThan(1e-6);
    const p = ex.savePose('stand');
    expect(p?.name).toBe('stand');
    ex.editPose(false);
    ex.setIdlePose('stand');
    expect(ex.settings.idlePose).toBe('stand');
    ex.setIdlePose(null);
    expect(ex.activePose).toBeNull();
    ex.setMorph('smile', 0.6);
    ex.saveFace('happy');
    ex.resetFace();
    expect(ex.settings.face).toEqual({});
    ex.applyFace('happy');
    expect(ex.settings.face).toEqual({ smile: 0.6 });
    expect(ex.morphs).toContain('smile');
    ex.dispose();
  });

  it('fires flick shortcuts: motions, faces and stamps', () => {
    const { stage } = fakeStage();
    const ex = attachStudioExtras(stage, { storage: null });
    ex.fireFlick({ action: { kind: 'motion', id: 'greet' } });
    expect(stage.gesture).toHaveBeenCalledWith('greet');
    ex.fireFlick({ action: { kind: 'face', id: 'surprised' } });
    expect(stage.setExpression).toHaveBeenCalledWith('surprised', 1);
    ex.fireFlick({ action: { kind: 'stamp', id: 'heart' } });
    expect(document.querySelector('.lucina-studio-overlay img')).toBeTruthy();
    ex.dispose();
  });

  it('a press-and-hold opens the flick ring and the flick direction fires', () => {
    vi.useFakeTimers();
    const { stage, ctx } = fakeStage();
    const ex = attachStudioExtras(stage, { storage: null });
    ex.setFlick('hold');
    const ev = (type: string, x: number, y: number) => {
      const e = new MouseEvent(type, { clientX: x, clientY: y, bubbles: true, cancelable: true }) as unknown as PointerEvent;
      Object.defineProperty(e, 'pointerId', { value: 7 });
      Object.defineProperty(e, 'isPrimary', { value: true });
      Object.defineProperty(e, 'pointerType', { value: 'touch' });
      ctx.canvas.dispatchEvent(e);
    };
    ev('pointerdown', 100, 100);
    vi.advanceTimersByTime(400);
    expect(stage.interactive).toBe(false);
    ev('pointermove', 180, 100); // right: slot 2 (wave)
    ev('pointerup', 180, 100);
    expect(stage.gesture).toHaveBeenCalledWith('greet');
    expect(stage.interactive).toBe(true);
    ex.dispose();
    vi.useRealTimers();
  });
});
