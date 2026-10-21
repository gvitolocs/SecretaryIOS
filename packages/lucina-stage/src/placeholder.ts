/**
 * Procedural stand-in for the Lucina model: a stylised bust/figure built from three.js primitives,
 * with a bone-like node hierarchy (spine/chest/neck/head/eye_l/eye_r) and real morph targets for the
 * mouth visemes, blinks and brows, so the whole pipeline runs without the private GLB.
 * Character faces +Z, feet at y = 0, about 1.72 units tall.
 */
import {
  BufferAttribute,
  BufferGeometry,
  CapsuleGeometry,
  CircleGeometry,
  Color,
  ConeGeometry,
  CylinderGeometry,
  DataTexture,
  DoubleSide,
  Float32BufferAttribute,
  Group,
  LatheGeometry,
  Mesh,
  MeshToonMaterial,
  NearestFilter,
  Object3D,
  RedFormat,
  SphereGeometry,
  TorusGeometry,
  Vector2,
  type Material,
} from 'three';

const COLORS = {
  skin: 0xf9dccb,
  hair: 0x22306a,
  iris: 0x3a74e0,
  irisDark: 0x1b3478,
  white: 0xfbfbff,
  mouth: 0x7a2638,
  outfit: 0x2c3f82,
  outfitDark: 0x1b2654,
  gold: 0xe0b84e,
  boots: 0x3a2a26,
  lash: 0x1a1830,
};

function gradientMap(): DataTexture {
  const data = new Uint8Array([90, 175, 255]);
  const t = new DataTexture(data, data.length, 1, RedFormat);
  t.minFilter = NearestFilter;
  t.magFilter = NearestFilter;
  t.generateMipmaps = false;
  t.needsUpdate = true;
  return t;
}

function lathe(profile: [number, number][], segments = 40): LatheGeometry {
  return new LatheGeometry(
    profile.map(([r, y]) => new Vector2(r, y)),
    segments,
  );
}

function setMorphs(geo: BufferGeometry, targets: Record<string, Float32Array>): void {
  const base = geo.getAttribute('position').array as Float32Array;
  const names = Object.keys(targets);
  geo.morphAttributes.position = names.map((n) => {
    const t = targets[n]!;
    const delta = new Float32Array(base.length);
    for (let i = 0; i < base.length; i++) delta[i] = (t[i] as number) - (base[i] as number);
    const attr = new Float32BufferAttribute(delta, 3);
    attr.name = n;
    return attr;
  });
  geo.morphTargetsRelative = true;
}

function morphMesh(geo: BufferGeometry, mat: Material, name: string, names: string[]): Mesh {
  const mesh = new Mesh(geo, mat);
  mesh.name = name;
  mesh.updateMorphTargets();
  // updateMorphTargets uses attribute names; make sure the dictionary matches our order.
  mesh.morphTargetDictionary = Object.fromEntries(names.map((n, i) => [n, i]));
  mesh.morphTargetInfluences = names.map(() => 0);
  return mesh;
}

/** Head profile (radius, height) from chin to crown, in head-local units. */
const HEAD_PROFILE: [number, number][] = [
  [0.0005, 0.005],
  [0.022, 0.008],
  [0.045, 0.022],
  [0.065, 0.048],
  [0.082, 0.08],
  [0.092, 0.11],
  [0.097, 0.14],
  [0.095, 0.175],
  [0.084, 0.205],
  [0.06, 0.228],
  [0.03, 0.24],
  [0.0005, 0.243],
];
const HEAD_DEPTH = 1.05;

function headRadiusAt(y: number): number {
  for (let i = 1; i < HEAD_PROFILE.length; i++) {
    const [r1, y1] = HEAD_PROFILE[i]!;
    const [r0, y0] = HEAD_PROFILE[i - 1]!;
    if (y <= y1) return r0 + ((r1 - r0) * (y - y0)) / (y1 - y0);
  }
  return 0;
}

/** Front surface z of the head at (x, y). */
function faceZ(x: number, y: number): number {
  const r = headRadiusAt(y);
  return Math.sqrt(Math.max(r * r - x * x, 0)) * HEAD_DEPTH;
}

// --- Eyes ---
const EYE = { x: 0.04, y: 0.115, rx: 0.021, ry: 0.026, rz: 0.01 };
const EYE_CZ = faceZ(EYE.x, EYE.y) - 0.005;

function lidZ(dx: number, dy: number): number {
  const e = 1 - (dx / (EYE.rx * 1.12)) ** 2 - (dy / (EYE.ry * 1.12)) ** 2;
  return EYE_CZ + EYE.rz * 1.12 * Math.sqrt(Math.max(e, 0)) + 0.0016;
}

function buildLids(): { geo: BufferGeometry; names: string[] } {
  const cols = 12;
  const skinRows = 6;
  const lashRows = 2;
  const top = EYE.ry + 0.008;
  const openEdge = EYE.ry - 0.002;
  const closedEdge = -EYE.ry - 0.002;
  const halfW = EYE.rx * 1.25;
  const lashH = 0.0055;
  const positions: number[] = [];
  const colors: number[] = [];
  const index: number[] = [];
  const targets: Record<string, number[]> = { blink: [], blink_l: [], blink_r: [] };
  const skin = new Color(COLORS.skin);
  const lash = new Color(COLORS.lash);
  const rows = skinRows + lashRows;

  for (const side of [1, -1]) {
    const cx = side * EYE.x;
    const start = positions.length / 3;
    for (let j = 0; j <= rows; j++) {
      for (let i = 0; i <= cols; i++) {
        const u = i / cols;
        const dx = (u * 2 - 1) * halfW;
        // Upper lid arc: the edge is higher in the middle.
        const arc = (1 - (dx / halfW) ** 2) * 0.004;
        const yFor = (edge: number): number => {
          if (j <= skinRows) return top + ((edge + arc - top) * j) / skinRows;
          return edge + arc - (lashH * (j - skinRows)) / lashRows;
        };
        const yOpen = yFor(openEdge);
        const yClosed = yFor(closedEdge - arc * 1.5);
        const xo = cx + dx;
        positions.push(xo, EYE.y + yOpen, lidZ(dx, yOpen));
        const closed = [xo, EYE.y + yClosed, lidZ(dx, yClosed)];
        const open = [xo, EYE.y + yOpen, lidZ(dx, yOpen)];
        targets.blink!.push(...closed);
        // Character's left eye is at +X.
        targets.blink_l!.push(...(side === 1 ? closed : open));
        targets.blink_r!.push(...(side === -1 ? closed : open));
        const c = j < skinRows ? skin : lash;
        colors.push(c.r, c.g, c.b);
      }
    }
    for (let j = 0; j < rows; j++) {
      for (let i = 0; i < cols; i++) {
        const a = start + j * (cols + 1) + i;
        const b = a + cols + 1;
        index.push(a, b, a + 1, a + 1, b, b + 1);
      }
    }
  }
  const geo = new BufferGeometry();
  geo.setAttribute('position', new Float32BufferAttribute(positions, 3));
  geo.setAttribute('color', new Float32BufferAttribute(colors, 3));
  geo.setIndex(index);
  geo.computeVertexNormals();
  const names = Object.keys(targets);
  setMorphs(
    geo,
    Object.fromEntries(names.map((n) => [n, new Float32Array(targets[n]!)])),
  );
  return { geo, names };
}

// --- Mouth ---
const MOUTH_Y = 0.047;
const MOUTH_Z = faceZ(0, MOUTH_Y) + 0.0012;

interface MouthShape {
  hw: number;
  up: number;
  low: number;
  /** Corner lift (positive = up). */
  corner?: number;
  dy?: number;
}

function mouthPositions(s: MouthShape, segs: number): number[] {
  const pos: number[] = [0, MOUTH_Y + (s.dy ?? 0) + ((s.corner ?? 0) * 0.15), MOUTH_Z + 0.0005];
  for (let k = 0; k <= segs; k++) {
    const th = (k / segs) * Math.PI * 2;
    const c = Math.cos(th);
    const sn = Math.sin(th);
    const x = s.hw * c;
    const y = (sn >= 0 ? s.up : s.low) * sn + (s.corner ?? 0) * c * c + (s.dy ?? 0);
    pos.push(x, MOUTH_Y + y, MOUTH_Z - x * x * 7);
  }
  return pos;
}

function buildMouth(): { geo: BufferGeometry; names: string[] } {
  const segs = 28;
  const base: MouthShape = { hw: 0.0125, up: 0.0009, low: 0.0013 };
  const shapes: Record<string, MouthShape> = {
    mouth_a: { hw: 0.0125, up: 0.006, low: 0.0145, dy: -0.002 },
    mouth_i: { hw: 0.0158, up: 0.0022, low: 0.0045 },
    mouth_u: { hw: 0.0068, up: 0.0045, low: 0.0055 },
    mouth_e: { hw: 0.0148, up: 0.0038, low: 0.0085, dy: -0.001 },
    mouth_o: { hw: 0.0088, up: 0.0068, low: 0.0105, dy: -0.0015 },
    smile: { hw: 0.0142, up: 0.0011, low: 0.0016, corner: 0.0042 },
    sad: { hw: 0.0115, up: 0.001, low: 0.0012, corner: -0.003 },
    surprised: { hw: 0.0075, up: 0.006, low: 0.0085, dy: -0.002 },
    angry: { hw: 0.0115, up: 0.0011, low: 0.002, corner: -0.0022 },
  };
  const geo = new BufferGeometry();
  geo.setAttribute('position', new Float32BufferAttribute(mouthPositions(base, segs), 3));
  const index: number[] = [];
  for (let k = 1; k <= segs; k++) index.push(0, k, k + 1);
  geo.setIndex(index);
  geo.setAttribute('normal', new Float32BufferAttribute(new Array((segs + 2) * 3).fill(0).map((_, i) => (i % 3 === 2 ? 1 : 0)), 3));
  const names = Object.keys(shapes);
  setMorphs(geo, Object.fromEntries(names.map((n) => [n, new Float32Array(mouthPositions(shapes[n]!, segs))])));
  return { geo, names };
}

// --- Brows ---
function buildBrows(): { geo: BufferGeometry; names: string[] } {
  const cols = 6;
  const len = 0.034;
  const h = 0.0042;
  const y0 = 0.158;
  const positions: number[] = [];
  const index: number[] = [];
  const targets: Record<string, number[]> = { angry: [], sad: [], surprised: [], smile: [] };
  for (const side of [1, -1]) {
    const start = positions.length / 3;
    for (let j = 0; j <= 1; j++) {
      for (let i = 0; i <= cols; i++) {
        const u = i / cols; // 0 = inner end, 1 = outer end
        const x = side * (0.022 + u * len);
        const arch = Math.sin(u * Math.PI) * 0.003 - u * 0.002;
        const thick = h * (1 - u * 0.55);
        const y = y0 + arch + (j === 1 ? thick : 0);
        const z = faceZ(x, y) + 0.0035;
        positions.push(x, y, z);
        const inner = 1 - u;
        const p = (dy: number): number[] => [x, y + dy, faceZ(x, y + dy) + 0.0035];
        targets.angry!.push(...p(-0.0065 * inner + 0.0015 * u));
        targets.sad!.push(...p(0.0055 * inner - 0.001 * u));
        targets.surprised!.push(...p(0.0065));
        targets.smile!.push(...p(0.0018));
      }
    }
    for (let i = 0; i < cols; i++) {
      const a = start + i;
      const b = a + cols + 1;
      index.push(a, a + 1, b, a + 1, b + 1, b);
    }
  }
  const geo = new BufferGeometry();
  geo.setAttribute('position', new Float32BufferAttribute(positions, 3));
  geo.setIndex(index);
  geo.computeVertexNormals();
  const names = Object.keys(targets);
  setMorphs(geo, Object.fromEntries(names.map((n) => [n, new Float32Array(targets[n]!)])));
  return { geo, names };
}

export interface Placeholder {
  root: Group;
}

export function createPlaceholder(): Placeholder {
  const grad = gradientMap();
  const toon = (color: number, extra: Partial<ConstructorParameters<typeof MeshToonMaterial>[0]> = {}): MeshToonMaterial =>
    new MeshToonMaterial({ color, gradientMap: grad, ...extra });
  const m = {
    skin: toon(COLORS.skin),
    hair: toon(COLORS.hair, { side: DoubleSide }),
    white: toon(COLORS.white),
    iris: toon(COLORS.iris, { polygonOffset: true, polygonOffsetFactor: -2, polygonOffsetUnits: -2 }),
    pupil: toon(COLORS.irisDark, { polygonOffset: true, polygonOffsetFactor: -3, polygonOffsetUnits: -3 }),
    shine: toon(0xffffff, { emissive: new Color(0xffffff), polygonOffset: true, polygonOffsetFactor: -4, polygonOffsetUnits: -4 }),
    mouth: toon(COLORS.mouth, { side: DoubleSide }),
    lids: toon(0xffffff, { vertexColors: true, side: DoubleSide }),
    outfit: toon(COLORS.outfit),
    outfitDark: toon(COLORS.outfitDark),
    gold: toon(COLORS.gold, { emissive: new Color(0x3a2a00) }),
    boots: toon(COLORS.boots),
    brows: toon(COLORS.hair, { side: DoubleSide }),
  };

  const mesh = (geo: BufferGeometry, mat: Material, name: string, parent: Object3D): Mesh => {
    const me = new Mesh(geo, mat);
    me.name = name;
    parent.add(me);
    return me;
  };
  const node = (name: string, parent: Object3D, y: number): Object3D => {
    const o = new Object3D();
    o.name = name;
    o.position.set(0, y, 0);
    parent.add(o);
    return o;
  };

  const root = new Group();
  root.name = 'LucinaPlaceholder';
  const spine = node('spine', root, 1.0);
  const chest = node('chest', spine, 0.22);
  const neck = node('neck', chest, 0.19);
  const head = node('head', neck, 0.075);

  // Legs, skirt and boots (only visible in full framing).
  for (const side of [1, -1]) {
    const leg = mesh(new CylinderGeometry(0.05, 0.042, 0.62, 16), m.outfitDark, `leg_${side}`, root);
    leg.position.set(side * 0.075, 0.36, 0);
    const boot = mesh(new CylinderGeometry(0.052, 0.058, 0.24, 16), m.boots, `boot_${side}`, root);
    boot.position.set(side * 0.075, 0.12, 0.005);
  }
  const skirt = mesh(lathe([[0.115, 0], [0.15, -0.18], [0.2, -0.36], [0.205, -0.4]]), m.outfit, 'skirt', spine);
  skirt.scale.z = 0.75;
  const belt = mesh(new TorusGeometry(0.113, 0.012, 8, 32), m.gold, 'belt', spine);
  belt.rotation.x = Math.PI / 2;
  belt.scale.y = 0.72;

  // Torso: lower half on the spine, upper half on the chest so breathing and leaning bend it.
  const lower = mesh(lathe([[0.11, -0.01], [0.112, 0.1], [0.135, 0.225]]), m.outfit, 'torso_lower', spine);
  lower.scale.z = 0.66;
  const upper = mesh(
    lathe([[0.135, 0.004], [0.155, 0.08], [0.165, 0.14], [0.15, 0.175], [0.085, 0.2], [0.042, 0.212]]),
    m.outfit,
    'torso_upper',
    chest,
  );
  upper.scale.z = 0.66;
  const collar = mesh(new TorusGeometry(0.05, 0.012, 8, 28), m.outfitDark, 'collar', chest);
  collar.position.y = 0.197;
  collar.rotation.x = Math.PI / 2;
  const clasp = mesh(new SphereGeometry(0.012, 12, 8), m.gold, 'clasp', chest);
  clasp.position.set(0, 0.17, 0.085);
  for (const side of [1, -1]) {
    const shoulder = mesh(new SphereGeometry(0.062, 20, 14), m.outfitDark, `pauldron_${side}`, chest);
    shoulder.position.set(side * 0.168, 0.16, 0);
    shoulder.scale.set(1, 0.75, 0.9);
    const arm = mesh(new CapsuleGeometry(0.038, 0.46, 6, 14), m.outfit, `arm_${side}`, chest);
    arm.position.set(side * 0.19, -0.1, 0);
    arm.rotation.z = side * 0.06;
    const hand = mesh(new SphereGeometry(0.036, 14, 10), m.skin, `hand_${side}`, chest);
    hand.position.set(side * 0.205, -0.38, 0.01);
  }
  const neckMesh = mesh(new CylinderGeometry(0.03, 0.035, 0.11, 18), m.skin, 'neck_mesh', neck);
  neckMesh.position.y = 0.04;

  // Head.
  const headMesh = mesh(lathe(HEAD_PROFILE, 48), m.skin, 'face', head);
  headMesh.scale.z = HEAD_DEPTH;
  const nose = mesh(new ConeGeometry(0.004, 0.012, 8), m.skin, 'nose', head);
  nose.position.set(0, 0.083, faceZ(0, 0.083) + 0.002);
  nose.rotation.x = 0.5;
  for (const side of [1, -1]) {
    const ear = mesh(new SphereGeometry(0.015, 10, 8), m.skin, `ear_${side}`, head);
    ear.position.set(side * 0.094, 0.105, -0.005);
    ear.scale.set(0.5, 1.2, 0.9);
  }

  // Eyes: the white is fixed to the head; the iris rotates with the eye node.
  for (const [side, name] of [
    [1, 'eye_l'],
    [-1, 'eye_r'],
  ] as const) {
    const white = mesh(new SphereGeometry(1, 24, 16), m.white, `${name}_white`, head);
    white.position.set(side * EYE.x, EYE.y, EYE_CZ);
    white.scale.set(EYE.rx, EYE.ry, EYE.rz);
    const pivot = new Object3D();
    pivot.name = name;
    pivot.position.set(side * EYE.x, EYE.y, EYE_CZ - 0.014);
    head.add(pivot);
    const r = 0.014 + EYE.rz + 0.0012;
    const iris = mesh(new CircleGeometry(0.0128, 28), m.iris, `${name}_iris`, pivot);
    iris.position.z = r;
    iris.scale.y = 1.22;
    const pupil = mesh(new CircleGeometry(0.0062, 20), m.pupil, `${name}_pupil`, pivot);
    pupil.position.z = r + 0.0003;
    pupil.scale.y = 1.25;
    const shine = mesh(new CircleGeometry(0.0034, 12), m.shine, `${name}_shine`, pivot);
    shine.position.set(0.004 * side, 0.0055, r + 0.0006);
  }
  const lids = buildLids();
  head.add(morphMesh(lids.geo, m.lids, 'Face_Lids', lids.names));
  const mouth = buildMouth();
  head.add(morphMesh(mouth.geo, m.mouth, 'Face_Mouth', mouth.names));
  const brows = buildBrows();
  head.add(morphMesh(brows.geo, m.brows, 'Face_Brows', brows.names));

  // Hair: cap, long back hair, side locks and bangs.
  const cap = mesh(new SphereGeometry(0.106, 36, 18, 0, Math.PI * 2, 0, Math.PI * 0.43), m.hair, 'hair_cap', head);
  cap.position.set(0, 0.137, -0.004);
  cap.scale.set(1.03, 1.02, 1.1);
  const back = mesh(
    new CylinderGeometry(0.108, 0.13, 0.42, 28, 1, true, Math.PI / 2 - 0.35, Math.PI + 0.7),
    m.hair,
    'hair_back',
    head,
  );
  back.position.set(0, -0.05, -0.01);
  back.scale.z = 1.05;
  for (const side of [1, -1]) {
    const lock = mesh(new ConeGeometry(0.02, 0.2, 10), m.hair, `hair_lock_${side}`, head);
    lock.position.set(side * 0.092, 0.07, 0.035);
    lock.rotation.set(0, 0, Math.PI + side * 0.08);
    lock.scale.z = 0.6;
  }
  const bangs: [number, number, number, number][] = [
    // x, y, length, roll
    [-0.066, 0.168, 0.085, -0.3],
    [-0.04, 0.178, 0.075, -0.18],
    [-0.014, 0.182, 0.06, -0.1],
    [0.012, 0.183, 0.066, 0.12],
    [0.038, 0.18, 0.074, 0.22],
    [0.064, 0.17, 0.088, 0.32],
  ];
  bangs.forEach(([x, y, len, roll], i) => {
    const b = mesh(new ConeGeometry(0.019, len, 8), m.hair, `hair_bang_${i}`, head);
    const yy = y - len / 2 + 0.01;
    b.position.set(x, yy, faceZ(x, yy + len * 0.25) + 0.006);
    b.rotation.set(-0.18, 0, Math.PI + roll);
    b.scale.z = 0.45;
  });
  // Tiara.
  const tiara = mesh(new TorusGeometry(0.104, 0.0042, 8, 48, Math.PI * 1.15), m.gold, 'tiara', head);
  // The arc lies in XY from +X counter-clockwise; Rx(pi/2 + tilt) lays it over the front, front lower.
  tiara.position.set(0, 0.19, -0.004);
  tiara.rotation.set(Math.PI / 2 + 0.25, 0, -Math.PI * 0.075, 'XYZ');
  tiara.scale.set(1, 1.08, 1);
  const jewel = mesh(new SphereGeometry(0.0075, 12, 8), m.gold, 'tiara_jewel', head);
  jewel.position.set(0, 0.164, 0.108);

  return { root };
}

/** Exposed for tests: number of vertices of the mouth (sanity checks on morph sizes). */
export function placeholderMouthVertexCount(): number {
  const g = buildMouth().geo;
  return (g.getAttribute('position') as BufferAttribute).count;
}
