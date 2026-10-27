/**
 * Studio layer (Custom Cast's Studio and Live sets, our own geometry): backgrounds (the stage set,
 * solid colours, three small rooms), lighting presets, and the dance-mode light show.
 *
 * - Rooms are open "dollhouse" sets around her: walls face inward and are back-face culled, so the
 *   orbiting camera always sees her through the near wall. Everything is plain three.js geometry
 *   and canvas textures drawn here; no assets.
 * - Lighting presets change the scene lights (PBR materials, the rooms) and, for toon materials
 *   (which ignore scene lights), their tint, light direction, ambient and rim colour.
 * - Dance mode: coloured spot beams sweeping from above, a pulsing floor glow, beat-synced rim
 *   colour on toon materials. In reduced motion the lights stay still.
 */
import {
  AdditiveBlending,
  BackSide,
  BoxGeometry,
  CanvasTexture,
  CircleGeometry,
  Color,
  ConeGeometry,
  CylinderGeometry,
  DoubleSide,
  FrontSide,
  Group,
  Mesh,
  MeshBasicMaterial,
  MeshStandardMaterial,
  PlaneGeometry,
  PointLight,
  RepeatWrapping,
  SphereGeometry,
  SRGBColorSpace,
  Vector3,
  type DirectionalLight,
  type HemisphereLight,
  type Material,
  type Object3D,
  type Scene,
  type ShaderMaterial,
  type Texture,
} from 'three';

export type RoomId = 'cafe' | 'bedroom' | 'rooftop';
export const ROOMS: readonly RoomId[] = ['cafe', 'bedroom', 'rooftop'];
/** `default` (the stage's own look), `transparent`, `color:#rrggbb`, or `room:<id>`. */
export type BackgroundId = string;
export const SOLID_COLORS = ['#101426', '#f4f1ec', '#ffd6e5', '#cfe8ff', '#1f3b2e'] as const;
export const BACKGROUNDS: readonly BackgroundId[] = ['default', ...SOLID_COLORS.map((c) => `color:${c}`), ...ROOMS.map((r) => `room:${r}`)];

export type LightingId = 'default' | 'studio' | 'warm' | 'cool' | 'sunset' | 'night';
export const LIGHTINGS: readonly LightingId[] = ['default', 'studio', 'warm', 'cool', 'sunset', 'night'];

interface LightPreset {
  key: [number, number];
  hemiSky: number;
  hemiGround: number;
  hemi: number;
  rim: [number, number];
  /** Toon: tint multiplier, light direction (character space), ambient offset, rim colour. */
  tint: number;
  dir: [number, number, number] | null;
  ambient: number;
  toonRim: number;
}

const PRESETS: Record<Exclude<LightingId, 'default'>, LightPreset> = {
  studio: { key: [0xffffff, 2.0], hemiSky: 0xffffff, hemiGround: 0x777788, hemi: 1.0, rim: [0xe8f0ff, 1.6], tint: 0xffffff, dir: [-0.2, 0.6, 0.78], ambient: 0.08, toonRim: 0xd8f4ff },
  warm: { key: [0xffd2a0, 1.9], hemiSky: 0xfff0dc, hemiGround: 0x6b4a3a, hemi: 0.9, rim: [0xffb878, 1.4], tint: 0xfff0dd, dir: [-0.45, 0.55, 0.7], ambient: 0.05, toonRim: 0xffc890 },
  cool: { key: [0xd2e6ff, 1.8], hemiSky: 0xe0ecff, hemiGround: 0x3d4a6b, hemi: 0.9, rim: [0x9fd0ff, 2.0], tint: 0xe9f1ff, dir: [0.3, 0.6, 0.74], ambient: 0.03, toonRim: 0x8fd8ff },
  sunset: { key: [0xff9a62, 2.1], hemiSky: 0xffc6a0, hemiGround: 0x4a2e52, hemi: 0.8, rim: [0xff6fa0, 2.2], tint: 0xffe2cc, dir: [-0.8, 0.25, 0.55], ambient: 0.02, toonRim: 0xff7fb0 },
  night: { key: [0x8fa8ff, 1.1], hemiSky: 0x5a6cb0, hemiGround: 0x141830, hemi: 0.55, rim: [0x7cc8ff, 2.4], tint: 0xb8c4f0, dir: [0.25, 0.8, 0.55], ambient: 0.0, toonRim: 0x6cb8ff },
};

export interface StudioDeps {
  scene: Scene;
  model: Object3D;
  lights: { hemi: HemisphereLight; key: DirectionalLight; rim: DirectionalLight; rim2: DirectionalLight };
  /** The stage set's own objects (sky, floor, particles), hidden for other backgrounds. */
  envObjects: () => Object3D[];
  /** Feet height, size, centre (world space). */
  floorY: number;
  height: number;
  center: Vector3;
}

interface ToonRef {
  mat: ShaderMaterial;
  tint: Color;
  dir: Vector3;
  ambient: number;
  rim: Color;
}

const TAU = Math.PI * 2;

function canvasTex(w: number, h: number, draw: (g: CanvasRenderingContext2D) => void): Texture | null {
  if (typeof document === 'undefined') return null;
  const c = document.createElement('canvas');
  c.width = w;
  c.height = h;
  const g = c.getContext('2d');
  if (!g) return null;
  draw(g);
  const t = new CanvasTexture(c);
  t.colorSpace = SRGBColorSpace;
  return t;
}

export class Studio {
  private bg: BackgroundId = 'default';
  private light: LightingId = 'default';
  private dance = false;
  private readonly rooms = new Map<RoomId, Group>();
  private readonly saved: { key: [Color, number]; rim: [Color, number]; rim2: [Color, number]; hemiSky: Color; hemiGround: Color; hemi: number };
  private savedBackground: unknown = null;
  private toons: ToonRef[] = [];
  private rescan = 0;
  private show: Group | null = null;
  private t = 0;
  reduced = false;

  constructor(private readonly d: StudioDeps) {
    const { key, rim, rim2, hemi } = d.lights;
    this.saved = {
      key: [key.color.clone(), key.intensity],
      rim: [rim.color.clone(), rim.intensity],
      rim2: [rim2.color.clone(), rim2.intensity],
      hemiSky: hemi.color.clone(),
      hemiGround: hemi.groundColor.clone(),
      hemi: hemi.intensity,
    };
  }

  get background(): BackgroundId {
    return this.bg;
  }

  get lighting(): LightingId {
    return this.light;
  }

  get dancing(): boolean {
    return this.dance;
  }

  setBackground(id: BackgroundId): void {
    if (!BACKGROUNDS.includes(id) && id !== 'transparent' && !/^color:#[0-9a-f]{6}$/i.test(id)) throw new TypeError(`Unknown background: ${id}`);
    const { scene } = this.d;
    if (this.bg === 'default') this.savedBackground = scene.background;
    this.bg = id;
    for (const g of this.rooms.values()) g.visible = false;
    const envVisible = id === 'default';
    for (const o of this.d.envObjects()) o.visible = envVisible;
    if (id === 'default') scene.background = this.savedBackground as Color | null;
    else if (id === 'transparent') scene.background = null;
    else if (id.startsWith('color:')) scene.background = new Color(id.slice(6));
    else if (id.startsWith('room:')) {
      const room = id.slice(5) as RoomId;
      scene.background = new Color(room === 'rooftop' ? 0x0b1030 : room === 'cafe' ? 0x2a1d16 : 0xf2e6ef);
      this.roomGroup(room).visible = true;
    }
  }

  setLighting(id: LightingId): void {
    if (!LIGHTINGS.includes(id)) throw new TypeError(`Unknown lighting: ${id}`);
    this.light = id;
    this.applyLights();
    this.scanToons();
    this.applyToons();
  }

  setDance(on: boolean): void {
    this.dance = !!on;
    if (this.dance) this.danceShow().visible = true;
    else if (this.show) {
      this.show.visible = false;
      this.applyToons();
    }
  }

  update(dt: number): void {
    this.t += dt;
    // Hair parts and outfit changes bring new toon materials: pick them up.
    this.rescan += dt;
    if (this.rescan > 1 && (this.light !== 'default' || this.dance)) {
      this.rescan = 0;
      if (this.scanToons()) this.applyToons();
    }
    if (this.dance && this.show) this.animateShow(dt);
  }

  dispose(): void {
    const mats = new Set<Material>();
    const texs = new Set<Texture>();
    for (const g of [...this.rooms.values(), ...(this.show ? [this.show] : [])]) {
      g.traverse((o) => {
        const m = o as Mesh;
        if (!m.isMesh) return;
        m.geometry.dispose();
        for (const mat of Array.isArray(m.material) ? m.material : [m.material]) {
          mats.add(mat);
          const map = (mat as MeshStandardMaterial).map;
          if (map) texs.add(map);
        }
      });
      g.removeFromParent();
    }
    mats.forEach((m) => m.dispose());
    texs.forEach((t) => t.dispose());
    this.rooms.clear();
    this.show = null;
  }

  // ---------- Lights ----------

  private applyLights(): void {
    const { key, rim, rim2, hemi } = this.d.lights;
    const s = this.saved;
    if (this.light === 'default') {
      key.color.copy(s.key[0]);
      key.intensity = s.key[1];
      rim.color.copy(s.rim[0]);
      rim.intensity = s.rim[1];
      rim2.color.copy(s.rim2[0]);
      rim2.intensity = s.rim2[1];
      hemi.color.copy(s.hemiSky);
      hemi.groundColor.copy(s.hemiGround);
      hemi.intensity = s.hemi;
      return;
    }
    const p = PRESETS[this.light];
    key.color.setHex(p.key[0]);
    key.intensity = p.key[1];
    rim.color.setHex(p.rim[0]);
    rim.intensity = p.rim[1];
    rim2.color.setHex(p.rim[0]);
    rim2.intensity = p.rim[1] * 0.5;
    hemi.color.setHex(p.hemiSky);
    hemi.groundColor.setHex(p.hemiGround);
    hemi.intensity = p.hemi;
  }

  /** Finds toon materials not seen before; true if any. */
  private scanToons(): boolean {
    const known = new Set(this.toons.map((r) => r.mat));
    let added = false;
    this.d.model.traverse((o) => {
      const m = o as Mesh;
      if (!m.isMesh) return;
      for (const mat of Array.isArray(m.material) ? m.material : [m.material]) {
        const u = (mat as ShaderMaterial).uniforms;
        if (!u?.tint || !u.lightDirection || known.has(mat as ShaderMaterial)) continue;
        known.add(mat as ShaderMaterial);
        this.toons.push({
          mat: mat as ShaderMaterial,
          tint: (u.tint.value as Color).clone(),
          dir: (u.lightDirection.value as Vector3).clone(),
          ambient: u.ambient?.value as number ?? 0,
          rim: (u.rimColor?.value as Color | undefined)?.clone() ?? new Color(0.212, 0.878, 1),
        });
        added = true;
      }
    });
    return added;
  }

  private applyToons(rimOverride?: Color): void {
    const p = this.light === 'default' ? null : PRESETS[this.light];
    const tint = p ? new Color(p.tint) : null;
    for (const r of this.toons) {
      const u = r.mat.uniforms;
      (u.tint!.value as Color).copy(r.tint);
      if (tint) (u.tint!.value as Color).multiply(tint);
      (u.lightDirection!.value as Vector3).copy(p?.dir ? new Vector3(...p.dir).normalize() : r.dir);
      if (u.ambient) u.ambient.value = r.ambient + (p?.ambient ?? 0);
      if (u.rimColor) (u.rimColor.value as Color).copy(rimOverride ?? (p ? new Color(p.toonRim) : r.rim));
    }
  }

  // ---------- Rooms ----------

  private roomGroup(id: RoomId): Group {
    let g = this.rooms.get(id);
    if (!g) {
      g = id === 'cafe' ? this.buildCafe() : id === 'bedroom' ? this.buildBedroom() : this.buildRooftop();
      g.name = `Room:${id}`;
      g.position.set(this.d.center.x, this.d.floorY, this.d.center.z);
      this.d.scene.add(g);
      this.rooms.set(id, g);
    }
    return g;
  }

  /** Floor, four inward-facing walls (culled from outside), sized to her height. */
  private shell(floor: MeshStandardMaterial, wall: MeshStandardMaterial, h: number, size = 4.2, wallH = 1.9): Group {
    const g = new Group();
    const W = size * h;
    const f = new Mesh(new PlaneGeometry(W, W), floor);
    f.rotation.x = -Math.PI / 2;
    f.position.y = -0.001;
    g.add(f);
    for (let i = 0; i < 4; i++) {
      const w = new Mesh(new PlaneGeometry(W, wallH * h), wall);
      const a = (i * Math.PI) / 2;
      w.position.set(Math.sin(a) * -W / 2, (wallH * h) / 2, Math.cos(a) * -W / 2);
      w.rotation.y = a;
      g.add(w);
    }
    return g;
  }

  private buildCafe(): Group {
    const h = this.d.height;
    const wood = canvasTex(256, 256, (c) => {
      for (let i = 0; i < 8; i++) {
        c.fillStyle = i % 2 ? '#7a5236' : '#8a5f3f';
        c.fillRect(0, i * 32, 256, 32);
        c.fillStyle = 'rgba(0,0,0,0.18)';
        c.fillRect(0, i * 32 + 31, 256, 1);
      }
    });
    if (wood) wood.repeat.set(6, 6), (wood.wrapS = wood.wrapT = RepeatWrapping);
    const brick = canvasTex(256, 256, (c) => {
      c.fillStyle = '#5a3a2e';
      c.fillRect(0, 0, 256, 256);
      for (let y = 0; y < 16; y++)
        for (let x = 0; x < 8; x++) {
          c.fillStyle = `hsl(${12 + ((x * 7 + y * 13) % 10)}, 38%, ${30 + ((x * 11 + y * 5) % 8)}%)`;
          c.fillRect(x * 32 + (y % 2) * 16 + 1, y * 16 + 1, 30, 14);
        }
    });
    if (brick) brick.repeat.set(5, 3), (brick.wrapS = brick.wrapT = RepeatWrapping);
    const g = this.shell(new MeshStandardMaterial({ map: wood, color: 0xffffff, roughness: 0.8 }), new MeshStandardMaterial({ map: brick, roughness: 0.95, side: FrontSide }), h);
    const counterMat = new MeshStandardMaterial({ color: 0x3b2a20, roughness: 0.6 });
    const counter = new Mesh(new BoxGeometry(2.2 * h, 0.62 * h, 0.4 * h), counterMat);
    counter.position.set(0.3 * h, 0.31 * h, -1.6 * h);
    g.add(counter);
    const top = new Mesh(new BoxGeometry(2.3 * h, 0.03 * h, 0.46 * h), new MeshStandardMaterial({ color: 0xd9cfc2, roughness: 0.3 }));
    top.position.set(0.3 * h, 0.635 * h, -1.6 * h);
    g.add(top);
    for (const x of [-0.9, 0.9]) {
      const stool = new Mesh(new CylinderGeometry(0.12 * h, 0.1 * h, 0.45 * h, 20), new MeshStandardMaterial({ color: 0x8c2f2f, roughness: 0.5 }));
      stool.position.set(x * h, 0.225 * h, -1.15 * h);
      g.add(stool);
    }
    for (const x of [-0.8, 0.3, 1.4]) {
      const bulb = new Mesh(new SphereGeometry(0.06 * h, 16, 12), new MeshBasicMaterial({ color: 0xffd9a0 }));
      bulb.position.set(x * h, 1.45 * h, -1.5 * h);
      g.add(bulb);
      const cord = new Mesh(new CylinderGeometry(0.004 * h, 0.004 * h, 0.4 * h, 4), new MeshBasicMaterial({ color: 0x222222 }));
      cord.position.set(x * h, 1.68 * h, -1.5 * h);
      g.add(cord);
    }
    const lamp = new PointLight(0xffc98a, 2.5, 4 * h, 1.5);
    lamp.position.set(0.3 * h, 1.4 * h, -1.2 * h);
    g.add(lamp);
    const win = new Mesh(new PlaneGeometry(1.1 * h, 0.7 * h), new MeshBasicMaterial({ color: 0xbfe3ff }));
    win.position.set(-2.09 * h, 1.05 * h, -0.6 * h);
    win.rotation.y = Math.PI / 2;
    g.add(win);
    return g;
  }

  private buildBedroom(): Group {
    const h = this.d.height;
    const g = this.shell(new MeshStandardMaterial({ color: 0xd9c3b0, roughness: 0.9 }), new MeshStandardMaterial({ color: 0xf6dbe6, roughness: 1 }), h);
    const rug = new Mesh(new CircleGeometry(0.9 * h, 48), new MeshStandardMaterial({ color: 0xb9d8f2, roughness: 1 }));
    rug.rotation.x = -Math.PI / 2;
    rug.position.y = 0.002;
    g.add(rug);
    const bed = new Mesh(new BoxGeometry(1.3 * h, 0.3 * h, 1.15 * h), new MeshStandardMaterial({ color: 0xffffff, roughness: 0.9 }));
    bed.position.set(-1.1 * h, 0.15 * h, -1.45 * h);
    g.add(bed);
    const blanket = new Mesh(new BoxGeometry(1.32 * h, 0.06 * h, 0.75 * h), new MeshStandardMaterial({ color: 0xe48ab0, roughness: 0.9 }));
    blanket.position.set(-1.1 * h, 0.31 * h, -1.25 * h);
    g.add(blanket);
    const pillow = new Mesh(new BoxGeometry(0.5 * h, 0.1 * h, 0.25 * h), new MeshStandardMaterial({ color: 0xfff6fa, roughness: 1 }));
    pillow.position.set(-1.1 * h, 0.34 * h, -1.85 * h);
    g.add(pillow);
    const shelf = new Mesh(new BoxGeometry(0.9 * h, 1.1 * h, 0.3 * h), new MeshStandardMaterial({ color: 0xf2efe9, roughness: 0.8 }));
    shelf.position.set(1.3 * h, 0.55 * h, -1.9 * h);
    g.add(shelf);
    const colors = [0x6d8fd8, 0xe4b04f, 0x7cc59b, 0xd86d8f];
    for (let i = 0; i < 8; i++) {
      const book = new Mesh(new BoxGeometry(0.06 * h, 0.22 * h, 0.2 * h), new MeshStandardMaterial({ color: colors[i % 4], roughness: 0.7 }));
      book.position.set((1.0 + i * 0.075) * h, (i < 4 ? 0.75 : 0.35) * h + 0.11 * h, -1.85 * h);
      g.add(book);
    }
    const win = new Mesh(new PlaneGeometry(0.9 * h, 0.8 * h), new MeshBasicMaterial({ color: 0xd8eeff }));
    win.position.set(0.1 * h, 1.15 * h, -2.09 * h);
    g.add(win);
    const lampLight = new PointLight(0xfff0e0, 1.6, 4 * h, 1.5);
    lampLight.position.set(0, 1.6 * h, 0.4 * h);
    g.add(lampLight);
    return g;
  }

  private buildRooftop(): Group {
    const h = this.d.height;
    const g = new Group();
    const tiles = canvasTex(128, 128, (c) => {
      c.fillStyle = '#3a3d4a';
      c.fillRect(0, 0, 128, 128);
      c.strokeStyle = '#2b2d38';
      c.lineWidth = 3;
      c.strokeRect(0, 0, 64, 64);
      c.strokeRect(64, 64, 64, 64);
      c.strokeRect(64, 0, 64, 64);
      c.strokeRect(0, 64, 64, 64);
    });
    if (tiles) tiles.repeat.set(12, 12), (tiles.wrapS = tiles.wrapT = RepeatWrapping);
    const floor = new Mesh(new PlaneGeometry(4.2 * h, 4.2 * h), new MeshStandardMaterial({ map: tiles, roughness: 0.9 }));
    floor.rotation.x = -Math.PI / 2;
    g.add(floor);
    // Sky dome with a vertical gradient (inside faces).
    const sky = canvasTex(4, 256, (c) => {
      const gr = c.createLinearGradient(0, 0, 0, 256);
      gr.addColorStop(0, '#05081c');
      gr.addColorStop(0.55, '#1b2560');
      gr.addColorStop(1, '#6b4a8e');
      c.fillStyle = gr;
      c.fillRect(0, 0, 4, 256);
    });
    const dome = new Mesh(new SphereGeometry(9 * h, 32, 16, 0, TAU, 0, Math.PI / 2), new MeshBasicMaterial({ map: sky, side: BackSide, fog: false }));
    g.add(dome);
    // Skyline: boxes with lit windows, in a ring.
    const windows = canvasTex(64, 128, (c) => {
      c.fillStyle = '#0d1126';
      c.fillRect(0, 0, 64, 128);
      for (let y = 4; y < 124; y += 10)
        for (let x = 4; x < 60; x += 10) {
          if ((x * 13 + y * 7) % 5 < 2) continue;
          c.fillStyle = (x + y) % 3 ? '#ffd27a' : '#9fd4ff';
          c.fillRect(x, y, 5, 6);
        }
    });
    const bmat = new MeshBasicMaterial({ map: windows, color: 0xbbbbbb });
    for (let i = 0; i < 26; i++) {
      const a = (i / 26) * TAU + 0.1;
      const r = (5.5 + ((i * 37) % 10) / 6) * h;
      const bh = (1.2 + ((i * 53) % 17) / 6) * h;
      const b = new Mesh(new BoxGeometry(0.8 * h, bh, 0.8 * h), bmat);
      b.position.set(Math.sin(a) * r, bh / 2 - 0.9 * h, Math.cos(a) * r);
      b.lookAt(0, b.position.y, 0);
      g.add(b);
    }
    const moon = new Mesh(new SphereGeometry(0.35 * h, 24, 16), new MeshBasicMaterial({ color: 0xfff6d8 }));
    moon.position.set(-3.5 * h, 4.5 * h, -6 * h);
    g.add(moon);
    // Railing.
    const rail = new MeshStandardMaterial({ color: 0x9aa3b8, metalness: 0.6, roughness: 0.4 });
    for (let i = 0; i < 4; i++) {
      const a = (i * Math.PI) / 2;
      const bar = new Mesh(new BoxGeometry(4.2 * h, 0.03 * h, 0.03 * h), rail);
      bar.position.set(Math.sin(a) * -2.1 * h, 0.62 * h, Math.cos(a) * -2.1 * h);
      bar.rotation.y = a;
      g.add(bar);
    }
    // String lights.
    const bulbMat = new MeshBasicMaterial({ color: 0xffe2a8 });
    for (let i = 0; i < 24; i++) {
      const t = i / 23;
      const b = new Mesh(new SphereGeometry(0.025 * h, 8, 6), bulbMat);
      b.position.set((-1.9 + 3.8 * t) * h, (1.75 - 0.25 * Math.sin(Math.PI * t)) * h, -1.9 * h);
      g.add(b);
    }
    return g;
  }

  // ---------- Dance light show ----------

  private danceShow(): Group {
    if (this.show) return this.show;
    const h = this.d.height;
    const g = new Group();
    g.name = 'DanceShow';
    g.position.set(this.d.center.x, this.d.floorY, this.d.center.z);
    const colors = [0xff3d9a, 0x3dd6ff, 0xa45bff, 0xffd23d];
    colors.forEach((c, i) => {
      const beam = new Mesh(
        new ConeGeometry(0.55 * h, 2.6 * h, 32, 1, true),
        new MeshBasicMaterial({ color: c, transparent: true, opacity: 0.16, blending: AdditiveBlending, depthWrite: false, side: DoubleSide }),
      );
      beam.geometry.translate(0, -1.3 * h, 0); // apex at the origin
      const pivot = new Group();
      const a = (i / colors.length) * TAU + Math.PI / 4;
      pivot.position.set(Math.sin(a) * 1.2 * h, 2.4 * h, Math.cos(a) * 0.9 * h - 0.3 * h);
      pivot.add(beam);
      pivot.userData = { phase: i * 1.7, color: new Color(c) };
      g.add(pivot);
    });
    const glow = new Mesh(
      new CircleGeometry(1.1 * h, 64),
      new MeshBasicMaterial({ color: 0xff3d9a, transparent: true, opacity: 0.25, blending: AdditiveBlending, depthWrite: false }),
    );
    glow.rotation.x = -Math.PI / 2;
    glow.position.y = 0.003;
    glow.name = 'DanceGlow';
    g.add(glow);
    this.d.scene.add(g);
    this.show = g;
    return g;
  }

  private animateShow(_dt: number): void {
    const g = this.show!;
    const t = this.reduced ? 0 : this.t;
    const beat = this.reduced ? 0.5 : Math.pow(1 - ((t * 2) % 1), 3); // 120 BPM
    for (const pivot of g.children) {
      if (!(pivot as Group).isGroup) continue;
      const ph = pivot.userData.phase as number;
      pivot.rotation.z = Math.sin(t * 0.9 + ph) * 0.55;
      pivot.rotation.x = Math.cos(t * 0.7 + ph * 1.3) * 0.35;
      const beam = pivot.children[0] as Mesh;
      (beam.material as MeshBasicMaterial).opacity = 0.08 + 0.12 * beat;
    }
    const glow = g.getObjectByName('DanceGlow') as Mesh | undefined;
    if (glow) {
      const k = Math.floor(t * 2) % 4;
      const col = [0xff3d9a, 0x3dd6ff, 0xa45bff, 0xffd23d][k]!;
      (glow.material as MeshBasicMaterial).color.setHex(col);
      (glow.material as MeshBasicMaterial).opacity = 0.12 + 0.25 * beat;
      this.applyToons(new Color(col));
    }
  }
}
