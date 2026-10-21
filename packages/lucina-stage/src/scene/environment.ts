/**
 * The stage set around Lucina: nebula sky, holographic floor ring, ambient motes, tap bursts,
 * neon rim lighting, gentle camera drift and (on the high tier) bloom.
 *
 * It reacts to the conversation state: listening rolls ripples out of the floor ring, thinking
 * swirls the motes and tints everything violet, speaking makes the halo, ring core and rim
 * lights follow her voice. `prefers-reduced-motion` freezes the drift, hides the motes and
 * disables bloom; the sky and ring stay, still.
 */
import { Color, Vector2, Vector3, type DirectionalLight, type HemisphereLight, type PerspectiveCamera, type Scene, type WebGLRenderer } from 'three';
import { damp } from '../spring';
import type { StageState } from '../types';
import { HoloFloor } from './floor';
import { Burst, Motes } from './particles';
import { BloomPipeline } from './post';
import { detectQuality, lowerQuality, QUALITY_PROFILES, type StageQuality, type StageQualitySetting, type StageTheme } from './quality';
import { Sky, type SkyPalette } from './sky';

export interface EnvironmentOptions {
  quality?: StageQualitySetting;
  theme?: StageTheme;
}

export interface EnvironmentSubject {
  /** Model height (character units). */
  height: number;
  /** Feet height. */
  floorY: number;
  /** Centre of the body footprint. */
  center: Vector3;
  /** Point on the chest used for the sky halo. */
  chest: Vector3;
  /** Horizontal half-extent of the body. */
  radius: number;
}

export interface EnvironmentLights {
  hemi: HemisphereLight;
  key: DirectionalLight;
  rim: DirectionalLight;
  rim2: DirectionalLight;
}

interface Palette {
  sky: SkyPalette;
  a: Color;
  b: Color;
  c: Color;
  rim: Color;
  rim2: Color;
  key: Color;
  hemiSky: Color;
  hemiGround: Color;
  exposure: number;
}

const PALETTES: Record<StageTheme, Palette> = {
  dark: {
    sky: {
      top: new Color(0x04061a),
      mid: new Color(0x150b38),
      bottom: new Color(0x030b14),
      nebA: new Color(0x13608a),
      nebB: new Color(0x5a1f86),
      halo: new Color(0x5a3fd0),
      light: 0,
    },
    a: new Color(0x3cf2ff),
    b: new Color(0xb36bff),
    c: new Color(0xff5fd2),
    rim: new Color(0x7fe9ff),
    rim2: new Color(0xff8ae0),
    key: new Color(0xfff2ea),
    hemiSky: new Color(0xe9e6ff),
    hemiGround: new Color(0x2a2f6a),
    exposure: 0.95,
  },
  light: {
    sky: {
      top: new Color(0xdfe6ff),
      mid: new Color(0xf6eefe),
      bottom: new Color(0xe4f6f7),
      nebA: new Color(0x8fd8ff),
      nebB: new Color(0xf3b6ff),
      halo: new Color(0xffffff),
      light: 1,
    },
    a: new Color(0x0aa6c4),
    b: new Color(0x6a4dff),
    c: new Color(0xd23bb0),
    rim: new Color(0xa6ecff),
    rim2: new Color(0xffc4ef),
    key: new Color(0xfff6f0),
    hemiSky: new Color(0xffffff),
    hemiGround: new Color(0x9aa2d6),
    exposure: 0.92,
  },
};

const _v = new Vector3();
const _d = new Vector3();

export class StageEnvironment {
  readonly sky = new Sky();
  readonly floor: HoloFloor;
  readonly motes: Motes;
  readonly burst: Burst;
  private bloom: BloomPipeline | null = null;
  private setting: StageQualitySetting;
  private tier: StageQuality;
  private theme: StageTheme;
  private reduced = false;
  private t = 0;
  private listen = 0;
  private think = 0;
  private level = 0;
  private tap = -1;
  private slow = 0;
  private dtAvg = 1 / 60;
  private size = { w: 1, h: 1, dpr: 1 };
  private readonly base: { key: number; rim: number; rim2: number; hemi: number };
  private readonly drift = new Vector3();
  private readonly center2 = new Vector2(0.5, 0.6);

  constructor(
    private readonly renderer: WebGLRenderer,
    private readonly scene: Scene,
    private readonly camera: PerspectiveCamera,
    private readonly subject: EnvironmentSubject,
    private readonly lights: EnvironmentLights,
    opts: EnvironmentOptions = {},
    /** Called when the DPR cap changes (quality switch), so the stage can resize. */
    private readonly onDprChange: () => void = () => {},
  ) {
    this.setting = opts.quality ?? 'auto';
    this.tier = this.setting === 'auto' ? detectQuality() : this.setting;
    this.theme = opts.theme ?? 'dark';
    const h = subject.height;
    this.floor = new HoloFloor(Math.max(subject.radius * 2.6, h * 0.5));
    this.floor.mesh.position.set(subject.center.x, subject.floorY + h * 0.002, subject.center.z);
    const maxMotes = QUALITY_PROFILES.high.particles;
    this.motes = new Motes(maxMotes, {
      origin: new Vector3(subject.center.x, subject.floorY, subject.center.z),
      radius: Math.max(subject.radius * 2.2, h * 0.38),
      height: h * 1.2,
      size: h * 0.0065,
    });
    this.burst = new Burst(56, h * 0.28, h * 0.011);
    scene.add(this.sky.mesh, this.floor.mesh, this.motes.points, this.burst.points);
    this.base = {
      key: lights.key.intensity,
      rim: lights.rim.intensity * 1.7,
      rim2: lights.rim2.intensity * 2.4,
      hemi: lights.hemi.intensity * 0.85,
    };
    this.applyTheme();
    this.applyTier();
  }

  get quality(): StageQuality {
    return this.tier;
  }

  get qualitySetting(): StageQualitySetting {
    return this.setting;
  }

  /** DPR cap for the current tier. */
  get maxPixelRatio(): number {
    return QUALITY_PROFILES[this.tier].dpr;
  }

  setQuality(q: StageQualitySetting): void {
    this.setting = q;
    this.tier = q === 'auto' ? detectQuality() : q;
    this.slow = 0;
    this.applyTier();
    this.onDprChange();
  }

  setTheme(theme: StageTheme): void {
    if (theme === this.theme) return;
    this.theme = theme;
    this.applyTheme();
  }

  setReducedMotion(reduced: boolean): void {
    if (reduced === this.reduced) return;
    this.reduced = reduced;
    this.applyTier();
  }

  /** Sparkle burst and floor shock ring, e.g. when she is tapped. Point in world space. */
  pulse(at?: { x: number; y: number; z: number }): void {
    this.tap = 0;
    if (this.reduced) return;
    const p = at ? _v.set(at.x, at.y, at.z) : _v.copy(this.subject.chest);
    this.burst.fire(p);
  }

  private applyTheme(): void {
    const p = PALETTES[this.theme];
    const light = this.theme === 'light';
    this.sky.setPalette(p.sky);
    this.floor.setColors(p.a, p.b, p.c, light);
    this.motes.setColors(p.a, p.b, p.c, light);
    this.burst.setColors(p.a, p.c, new Color(0xffe27a), light);
    this.lights.rim.color.copy(p.rim);
    this.lights.rim2.color.copy(p.rim2);
    this.lights.key.color.copy(p.key);
    this.lights.hemi.color.copy(p.hemiSky);
    this.lights.hemi.groundColor.copy(p.hemiGround);
    this.renderer.toneMappingExposure = p.exposure;
    this.bloom?.setTheme(light);
    this.applyTier();
  }

  private applyTier(): void {
    const prof = QUALITY_PROFILES[this.tier];
    this.sky.material.uniforms.uDetail!.value = prof.skyDetail;
    this.motes.setCount(this.reduced ? 0 : prof.particles);
    // Bloom on a bright sky would haze everything: dark theme only.
    const wantBloom = prof.bloom && !this.reduced && this.theme === 'dark';
    if (wantBloom && !this.bloom) {
      try {
        this.bloom = new BloomPipeline(this.renderer, this.scene, this.camera);
        this.bloom.setTheme(this.theme === 'light');
        this.bloom.setSize(this.size.w, this.size.h, this.size.dpr);
      } catch (e) {
        console.warn('[lucina-stage] bloom unavailable:', e);
        this.bloom = null;
      }
    } else if (!wantBloom && this.bloom) {
      this.bloom.dispose();
      this.bloom = null;
    }
  }

  setSize(w: number, h: number, dpr: number): void {
    this.size = { w, h, dpr };
    this.sky.material.uniforms.uAspect!.value = w / Math.max(h, 1);
    this.bloom?.setSize(w, h, dpr);
  }

  /**
   * Per frame, before rendering. `level` is her voice openness (0..1).
   */
  update(dt: number, state: StageState, level: number): void {
    const still = this.reduced;
    if (!still) this.t += dt;
    const t = this.t;
    this.listen = damp(this.listen, state === 'listening' ? 1 : 0, 0.25, dt);
    this.think = damp(this.think, state === 'thinking' ? 1 : 0, 0.4, dt);
    this.level = damp(this.level, state === 'speaking' ? level : 0, 0.08, dt);
    if (this.tap >= 0) {
      this.tap += dt / 0.9;
      if (this.tap >= 1) this.tap = -1;
    }
    this.burst.update(dt);

    const su = this.sky.material.uniforms;
    su.uTime!.value = t;
    su.uListen!.value = this.listen;
    su.uThink!.value = this.think;
    su.uLevel!.value = this.level;
    _d.copy(this.subject.chest).project(this.camera);
    this.center2.set(_d.x * 0.5 + 0.5, _d.y * 0.5 + 0.5);
    (su.uCenter!.value as Vector2).copy(this.center2);
    (su.uParallax!.value as Vector2).set(this.drift.x * 0.6, this.drift.y * 0.6);

    const fu = this.floor.material.uniforms;
    fu.uTime!.value = t;
    fu.uListen!.value = this.listen;
    fu.uThink!.value = this.think;
    fu.uLevel!.value = this.level;
    fu.uTap!.value = this.tap < 0 ? 0 : this.tap;

    const mu = this.motes.material.uniforms;
    mu.uTime!.value = t;
    mu.uSwirl!.value = this.think;
    const pixel = (this.size.h * this.size.dpr) / (2 * Math.tan((this.camera.fov * Math.PI) / 360));
    mu.uPixel!.value = pixel;
    this.burst.material.uniforms.uPixel!.value = pixel;

    // Neon rims follow her voice; a violet cast while thinking.
    const L = this.lights;
    L.rim.intensity = this.base.rim * (1 + 0.5 * this.level + 0.2 * this.listen);
    L.rim2.intensity = this.base.rim2 * (1 + 0.9 * this.level + 0.3 * this.think);
    L.key.intensity = this.base.key * (1 + 0.06 * this.level);
    L.hemi.intensity = this.base.hemi;
    this.bloom?.setBoost(this.level);

    // `auto` steps down when frames stay slow (> 28 ms) for ~4 s.
    this.dtAvg += (dt - this.dtAvg) * 0.05;
    if (this.setting === 'auto' && dt > 0) {
      this.slow = this.dtAvg > 0.028 ? this.slow + dt : Math.max(0, this.slow - dt * 0.5);
      if (this.slow > 4) {
        const next = lowerQuality(this.tier);
        this.slow = 0;
        if (next) {
          this.tier = next;
          this.applyTier();
          this.onDprChange();
        }
      }
    }
  }

  /** Gentle orbit-like camera drift around the framing target (call after the camera is placed). */
  driftCamera(camera: PerspectiveCamera, target: Vector3): void {
    if (this.reduced) {
      this.drift.set(0, 0, 0);
      return;
    }
    const t = this.t;
    const dist = camera.position.distanceTo(target);
    this.drift.set(
      Math.sin(t * 0.09) * 0.022 + Math.sin(t * 0.23 + 1.3) * 0.006,
      Math.sin(t * 0.07 + 0.6) * 0.01,
      0,
    );
    // Sideways and vertical offsets in camera space, scaled by the distance.
    _v.set(1, 0, 0).applyQuaternion(camera.quaternion).multiplyScalar(this.drift.x * dist);
    camera.position.add(_v);
    _v.set(0, 1, 0).applyQuaternion(camera.quaternion).multiplyScalar(this.drift.y * dist);
    camera.position.add(_v);
    camera.lookAt(target);
  }

  render(dt: number): void {
    if (this.bloom) this.bloom.render(dt);
    else this.renderer.render(this.scene, this.camera);
  }

  dispose(): void {
    this.bloom?.dispose();
    this.bloom = null;
    this.scene.remove(this.sky.mesh, this.floor.mesh, this.motes.points, this.burst.points);
    this.sky.dispose();
    this.floor.dispose();
    this.motes.dispose();
    this.burst.dispose();
  }
}
