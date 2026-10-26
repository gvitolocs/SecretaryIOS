/**
 * Orbit camera for touch and mouse, after Custom Cast's `UltimateOrbitCamera` (see
 * docs/design/custom-cast-interactions.md, section 1):
 *
 * - **Rotate:** a drag adds to an angular velocity, the angle integrates it, and the velocity
 *   decays by ×0.833 (yaw) / ×0.839 (pitch) per 60 Hz frame. The view lags the finger a little
 *   while you drag and glides about 0.3 s after you let go. Yaw is free; pitch is clamped
 *   (−30°…+45°, SceneLive's limits).
 * - **Zoom:** one value along a path from full body (0) through bust (0.5) to face (1). Target,
 *   distance and FOV blend between the three framings, so zooming in walks up to her face. It eases
 *   toward the goal by 0.3 per frame (`smoothingZoom`) and rubber-bands a little past the ends.
 * - **Pan:** moves the orbit target in the camera plane, eased by 0.3 per frame (`smoothingMove`),
 *   clamped to her bounding box.
 * - **Fly-to:** reset and presets animate every channel together (0.45 s, ease in-out).
 *
 * Pure math (no three.js objects, no DOM): unit-tested in test/interact.test.ts.
 */

export interface V3 {
  x: number;
  y: number;
  z: number;
}

/** One camera framing in world space. */
export interface Shot {
  target: V3;
  pos: V3;
  fov: number;
}

export type ViewPreset = 'full' | 'bust' | 'face';

/** Zoom values of the presets on the full → bust → face path. */
export const PRESET_ZOOM: Record<ViewPreset, number> = { full: 0, bust: 0.5, face: 1 };

/** Where the user has moved the view, relative to the framing. Angles in radians. */
export interface ViewState {
  /** Orbit around the vertical axis; 0 = the framing's own direction (in front of her). */
  yaw: number;
  /** Added elevation; positive = camera higher, looking down. */
  pitch: number;
  /** 0 = full body, 0.5 = bust, 1 = face. */
  zoom: number;
  /** Target offset in metres: x along the camera's right, y world up. */
  panX: number;
  panY: number;
}

export interface OrbitLimits {
  /** Lowest/highest total camera elevation (radians), framing elevation included. */
  minElevation: number;
  maxElevation: number;
  /** How far zoom may overshoot [0, 1] while a gesture holds it (rubber band). */
  overshoot: number;
}

export const DEFAULT_LIMITS: OrbitLimits = {
  minElevation: (-30 * Math.PI) / 180,
  maxElevation: (45 * Math.PI) / 180,
  overshoot: 0.06,
};

/** Custom Cast's per-frame constants (at 60 Hz). */
export const FEEL = {
  dampYaw: 0.833,
  dampPitch: 0.839,
  smoothZoom: 0.3,
  smoothPan: 0.3,
  /** Degrees a full-width horizontal drag turns her (Custom Cast's gain feels like this on a phone). */
  degreesPerWidth: 240,
  flyDuration: 0.45,
};

/** Bounds the pan keeps the target inside (world space), and the subject's size. */
export interface SubjectBounds {
  min: V3;
  max: V3;
  height: number;
}

const TAU = Math.PI * 2;

export function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}

/** Angle wrapped to (−π, π]. */
export function wrapAngle(a: number): number {
  const w = ((((a + Math.PI) % TAU) + TAU) % TAU) - Math.PI;
  return w === -Math.PI ? Math.PI : w;
}

/** Per-frame factor `k` (at 60 Hz) converted to a time step. Lerp weight for `dt` seconds. */
export function frameLerp(k: number, dt: number): number {
  return 1 - Math.pow(1 - k, dt * 60);
}

/** Per-frame decay `d` (at 60 Hz) over `dt` seconds. */
export function frameDecay(d: number, dt: number): number {
  return Math.pow(d, dt * 60);
}

function lerp(a: number, b: number, t: number): number {
  return a + (b - a) * t;
}

function lerpV(a: V3, b: V3, t: number): V3 {
  return { x: lerp(a.x, b.x, t), y: lerp(a.y, b.y, t), z: lerp(a.z, b.z, t) };
}

function sub(a: V3, b: V3): V3 {
  return { x: a.x - b.x, y: a.y - b.y, z: a.z - b.z };
}

function len(v: V3): number {
  return Math.hypot(v.x, v.y, v.z);
}

export function easeInOut(t: number): number {
  const x = clamp(t, 0, 1);
  return x < 0.5 ? 4 * x * x * x : 1 - Math.pow(-2 * x + 2, 3) / 2;
}

/** Spherical description of a shot: distance, base yaw and elevation of the camera around the target. */
export function shotSpherical(s: Shot): { dist: number; yaw: number; elevation: number } {
  const d = sub(s.pos, s.target);
  const dist = Math.max(len(d), 1e-4);
  return { dist, yaw: Math.atan2(d.x, d.z), elevation: Math.asin(clamp(d.y / dist, -1, 1)) };
}

/**
 * The shot at zoom `z` on the full (0) → bust (0.5) → face (1) path: target and FOV blend
 * linearly, distance geometrically (an even pinch feels even), elevation linearly.
 */
export function shotAt(shots: Record<ViewPreset, Shot>, z: number): { target: V3; dist: number; yaw: number; elevation: number; fov: number } {
  const zc = clamp(z, 0, 1);
  const [a, b, t] = zc <= 0.5 ? [shots.full, shots.bust, zc / 0.5] : [shots.bust, shots.face, (zc - 0.5) / 0.5];
  const sa = shotSpherical(a);
  const sb = shotSpherical(b);
  let dist = Math.exp(lerp(Math.log(sa.dist), Math.log(sb.dist), t));
  // Beyond the ends (rubber band): keep scaling the distance a little.
  if (z < 0) dist *= Math.exp(-z * 0.8);
  else if (z > 1) dist *= Math.exp(-(z - 1) * 0.8);
  return {
    target: lerpV(a.target, b.target, t),
    dist,
    yaw: lerp(sa.yaw, sb.yaw, t),
    elevation: lerp(sa.elevation, sb.elevation, t),
    fov: lerp(a.fov, b.fov, t),
  };
}

/** log(distance at full / distance at face): how much pinch ratio spans the whole zoom range. */
export function zoomSpan(shots: Record<ViewPreset, Shot>): number {
  const full = shotSpherical(shots.full).dist;
  const face = shotSpherical(shots.face).dist;
  return Math.max(Math.log(full / Math.max(face, 1e-4)), 0.2);
}

/** Camera position, target and FOV for a view over the framing shots. */
export function composeCamera(shots: Record<ViewPreset, Shot>, v: ViewState, limits: OrbitLimits = DEFAULT_LIMITS): Shot {
  const base = shotAt(shots, v.zoom);
  const yaw = base.yaw + v.yaw;
  const elevation = clamp(base.elevation + v.pitch, limits.minElevation, limits.maxElevation);
  // Pan: x along the camera's right (perpendicular to the view yaw), y world up.
  const target = {
    x: base.target.x + Math.cos(yaw) * v.panX,
    y: base.target.y + v.panY,
    z: base.target.z - Math.sin(yaw) * v.panX,
  };
  const c = Math.cos(elevation);
  return {
    target,
    pos: {
      x: target.x + base.dist * Math.sin(yaw) * c,
      y: target.y + base.dist * Math.sin(elevation),
      z: target.z + base.dist * Math.cos(yaw) * c,
    },
    fov: base.fov,
  };
}

/** The pitch range allowed at zoom `z` (so the total elevation stays inside the limits). */
export function pitchRange(shots: Record<ViewPreset, Shot>, z: number, limits: OrbitLimits = DEFAULT_LIMITS): [number, number] {
  const e = shotAt(shots, z).elevation;
  return [limits.minElevation - e, limits.maxElevation - e];
}

/** Clamps the pan so the orbit target stays on her (inside the bounds, a little margin). */
export function clampPan(shots: Record<ViewPreset, Shot>, v: ViewState, bounds: SubjectBounds | null): { panX: number; panY: number } {
  if (!bounds) return { panX: v.panX, panY: v.panY };
  const base = shotAt(shots, v.zoom);
  const halfW = Math.max((bounds.max.x - bounds.min.x) / 2, (bounds.max.z - bounds.min.z) / 2, bounds.height * 0.12);
  const panX = clamp(v.panX, -halfW, halfW);
  const panY = clamp(v.panY, bounds.min.y - base.target.y, bounds.max.y - base.target.y);
  return { panX, panY };
}

export interface FlyTo {
  from: ViewState;
  to: ViewState;
  t: number;
  duration: number;
}

/**
 * The live orbit: the view, its goals and velocities. Feed it gestures, call `update(dt)` once per
 * frame, read `view`.
 */
export class Orbit {
  /** What the camera shows now. */
  readonly view: ViewState;
  /** Angular velocities, radians per 60 Hz frame. */
  private vYaw = 0;
  private vPitch = 0;
  private zoomGoal: number;
  private panGoal = { x: 0, y: 0 };
  private fly: FlyTo | null = null;
  /** A pinch or wheel is holding the zoom (rubber band allowed). */
  private zoomHeld = false;
  reduced = false;
  limits: OrbitLimits = DEFAULT_LIMITS;
  shots: Record<ViewPreset, Shot>;
  bounds: SubjectBounds | null = null;
  /** The view a reset returns to. */
  home: ViewState;

  constructor(shots: Record<ViewPreset, Shot>, home: Partial<ViewState> = {}) {
    this.shots = shots;
    this.home = { yaw: 0, pitch: 0, zoom: 0.5, panX: 0, panY: 0, ...home };
    this.view = { ...this.home };
    this.zoomGoal = this.view.zoom;
  }

  /** The view is moving (a glide, a zoom ease or a fly-to). */
  get moving(): boolean {
    return (
      !!this.fly ||
      Math.abs(this.vYaw) > 1e-5 ||
      Math.abs(this.vPitch) > 1e-5 ||
      Math.abs(this.zoomGoal - this.view.zoom) > 1e-4 ||
      Math.abs(this.panGoal.x - this.view.panX) > 1e-5 ||
      Math.abs(this.panGoal.y - this.view.panY) > 1e-5
    );
  }

  get flying(): boolean {
    return !!this.fly;
  }

  /** A drag of (dx, dy) CSS px on a canvas `width` px wide. Dragging right turns her to the right. */
  rotate(dx: number, dy: number, width: number): void {
    this.fly = null;
    const perPx = ((FEEL.degreesPerWidth * Math.PI) / 180 / Math.max(width, 120)) * (1 - FEEL.dampYaw);
    if (this.reduced) {
      // No glide: move by the whole amount the velocity would have added up to.
      this.view.yaw = wrapAngle(this.view.yaw - (dx * perPx) / (1 - FEEL.dampYaw));
      this.view.pitch = this.clampPitch(this.view.pitch + (dy * perPx) / (1 - FEEL.dampPitch));
      return;
    }
    this.vYaw -= dx * perPx;
    this.vPitch += dy * perPx;
  }

  /** Multiplies the camera distance by 1/ratio (a pinch opening by `ratio`). */
  pinch(ratio: number): void {
    if (!(ratio > 0) || !Number.isFinite(ratio)) return;
    this.fly = null;
    this.zoomHeld = true;
    this.zoomGoal = this.clampZoom(this.zoomGoal + Math.log(ratio) / zoomSpan(this.shots), true);
    if (this.reduced) this.view.zoom = this.zoomGoal;
  }

  /** Zoom by a step (wheel: positive = in). */
  zoomBy(step: number): void {
    this.fly = null;
    this.zoomGoal = this.clampZoom(this.zoomGoal + step, false);
    if (this.reduced) this.view.zoom = this.zoomGoal;
  }

  /** Pans by a drag of (dx, dy) CSS px on a canvas `heightPx` tall: the target follows the fingers. */
  pan(dx: number, dy: number, heightPx: number): void {
    this.fly = null;
    const s = shotAt(this.shots, this.view.zoom);
    const metresPerPx = (2 * s.dist * Math.tan(((s.fov / 2) * Math.PI) / 180)) / Math.max(heightPx, 1);
    const next = clampPan(this.shots, { ...this.view, panX: this.panGoal.x - dx * metresPerPx, panY: this.panGoal.y + dy * metresPerPx }, this.bounds);
    this.panGoal = { x: next.panX, y: next.panY };
    if (this.reduced) {
      this.view.panX = this.panGoal.x;
      this.view.panY = this.panGoal.y;
    }
  }

  /** A gesture that held the zoom ended: spring back inside [0, 1]. */
  release(): void {
    this.zoomHeld = false;
    this.zoomGoal = clamp(this.zoomGoal, 0, 1);
    if (this.reduced) this.view.zoom = this.zoomGoal;
  }

  /** Stops the glide at once (a new touch grabs the view, as in Custom Cast). */
  grab(): void {
    this.vYaw = 0;
    this.vPitch = 0;
  }

  /** Animates (or jumps, in reduced motion / `animate` false) to a view. */
  flyTo(to: Partial<ViewState>, animate = true): void {
    const target: ViewState = { ...this.view, ...to };
    target.zoom = clamp(target.zoom, 0, 1);
    const pr = pitchRange(this.shots, target.zoom, this.limits);
    target.pitch = clamp(target.pitch, pr[0], pr[1]);
    // Take the short way round.
    target.yaw = this.view.yaw + wrapAngle(target.yaw - this.view.yaw);
    this.vYaw = this.vPitch = 0;
    this.zoomHeld = false;
    if (!animate || this.reduced) {
      Object.assign(this.view, target);
      this.view.yaw = wrapAngle(this.view.yaw);
      this.fly = null;
    } else {
      this.fly = { from: { ...this.view }, to: target, t: 0, duration: FEEL.flyDuration };
    }
    this.zoomGoal = target.zoom;
    this.panGoal = { x: target.panX, y: target.panY };
  }

  /** Back to the home view (double-tap). */
  reset(animate = true): void {
    this.flyTo(this.home, animate);
  }

  /** A preset keeps the orbit angle; it re-centres and sets the zoom. */
  preset(p: ViewPreset, animate = true): void {
    this.flyTo({ zoom: PRESET_ZOOM[p], panX: 0, panY: 0 }, animate);
  }

  update(dt: number): void {
    const v = this.view;
    if (this.fly) {
      const f = this.fly;
      f.t += dt;
      const k = easeInOut(f.t / f.duration);
      v.yaw = lerp(f.from.yaw, f.to.yaw, k);
      v.pitch = lerp(f.from.pitch, f.to.pitch, k);
      v.zoom = lerp(f.from.zoom, f.to.zoom, k);
      v.panX = lerp(f.from.panX, f.to.panX, k);
      v.panY = lerp(f.from.panY, f.to.panY, k);
      if (f.t >= f.duration) {
        this.fly = null;
        v.yaw = wrapAngle(v.yaw);
      }
      return;
    }
    if (this.vYaw || this.vPitch) {
      // Exact sum of the per-frame series over `steps` 60 Hz frames (frame-rate independent).
      const dy = frameDecay(FEEL.dampYaw, dt);
      const dp = frameDecay(FEEL.dampPitch, dt);
      v.yaw = wrapAngle(v.yaw + (this.vYaw * (1 - dy)) / (1 - FEEL.dampYaw));
      v.pitch = this.clampPitch(v.pitch + (this.vPitch * (1 - dp)) / (1 - FEEL.dampPitch));
      this.vYaw *= dy;
      this.vPitch *= dp;
      if (Math.abs(this.vYaw) < 1e-5) this.vYaw = 0;
      if (Math.abs(this.vPitch) < 1e-5) this.vPitch = 0;
    }
    v.zoom = lerp(v.zoom, this.zoomGoal, frameLerp(FEEL.smoothZoom, dt));
    if (Math.abs(v.zoom - this.zoomGoal) < 1e-4) v.zoom = this.zoomGoal;
    const pk = frameLerp(FEEL.smoothPan, dt);
    v.panX = lerp(v.panX, this.panGoal.x, pk);
    v.panY = lerp(v.panY, this.panGoal.y, pk);
    // Zooming changes the allowed pitch and pan ranges.
    v.pitch = this.clampPitch(v.pitch);
    if (!this.zoomHeld) {
      const p = clampPan(this.shots, { ...v, panX: this.panGoal.x, panY: this.panGoal.y }, this.bounds);
      this.panGoal = { x: p.panX, y: p.panY };
    }
  }

  /** Replace the framings (resize, framing change) keeping the user's view. */
  setShots(shots: Record<ViewPreset, Shot>, bounds?: SubjectBounds | null): void {
    this.shots = shots;
    if (bounds !== undefined) this.bounds = bounds;
  }

  private clampPitch(p: number): number {
    const [lo, hi] = pitchRange(this.shots, this.view.zoom, this.limits);
    return clamp(p, lo, hi);
  }

  private clampZoom(z: number, rubber: boolean): number {
    const o = rubber ? this.limits.overshoot : 0;
    return clamp(z, -o, 1 + o);
  }
}

/**
 * How much her gaze follows a camera at `yaw` (radians, 0 = in front of her): fully up to 100°,
 * fading to straight ahead at 150°, so she never snaps from one shoulder to the other.
 */
export function gazeFollowWeight(yaw: number): number {
  const a = Math.abs(wrapAngle(yaw));
  const lo = (100 * Math.PI) / 180;
  const hi = (150 * Math.PI) / 180;
  const x = clamp((hi - a) / (hi - lo), 0, 1);
  return x * x * (3 - 2 * x);
}
