/**
 * Studio extras on a stage (Custom Cast's Studio, the "later" rows of
 * docs/design/custom-cast-interactions.md): hand shapes (S2), IK pose editing and saved poses
 * (S3), face morph sliders and saved faces (S5), props (S7), photo decorations (P4), flick
 * shortcuts (D6), live stamps (D7), camera roll (C9), camera filters (C12), gaze sliders (E3),
 * an imported image background (B3) and procedural dance moves.
 *
 *   const extras = attachStudioExtras(stage, { propsUrl: '/studio/lucina-props.glb' });
 *
 * Everything runs in the stage's frame hook (src/studio2/hooks.ts), after the clips and before the
 * render, so it works the same on a plain stage and on a wardrobe stage across outfit swaps.
 * Settings (not the background image, which stays with the host) are kept in `storage`.
 */
import { Raycaster, SRGBColorSpace, Texture, Vector2, Vector3, type Object3D, type PerspectiveCamera } from 'three';
import type { LucinaStage } from '../stage';
import { findHumanoid, type Humanoid, type Side } from './bones';
import { DanceLayer, sampleDance, DANCE_MOVES, type DanceMove } from './dances';
import { openPhotoDecorator, type DecoratorOptions } from './decorate';
import { cleanMorphs, FaceMixer, listMorphs, type SavedFace } from './face';
import { cssFilter, filterBlob, normalizeFilter, vignetteCss, type FilterState } from './filters';
import { applyHand, bindHand, HAND_PRESETS, HAND_SHAPES, releaseHand, type HandBindingT, type HandShape } from './hands';
import type { FrameContext } from './hooks';
import { DEFAULT_FLICK_SLOTS, FlickRing, popStamp, type FlickSlot } from './overlay';
import { cleanPose, PoseEditor, PoseLayer, type SavedPose } from './poser';
import { clampProp, MAX_PROPS, PropLibrary, PropSet, type PropState } from './props';
import { restPose, withRestPose } from './rest';
import { STAMP_IDS } from './stamps';

export type FlickMode = 'off' | 'hold' | 'left';
export type GazeMode = 'camera' | 'ahead' | 'custom';
export interface GazeState {
  mode: GazeMode;
  /** -1..1: left/right and down/up (custom mode). */
  x: number;
  y: number;
}
export const GAZE_RANGE = { yaw: 0.7, pitch: 0.45 };
export const ROLL_LIMIT = 45;

export interface ExtrasStorage {
  get(key: string): string | null;
  set(key: string, value: string): void;
}

export interface StudioExtrasOptions {
  /** The props GLB (assets/props/lucina-props.glb, served by the host). Loaded on first use. */
  propsUrl?: string;
  fetchInit?: RequestInit;
  /** Where settings live (default: localStorage, key `secretary.studio2`); null: nowhere. */
  storage?: ExtrasStorage | null;
  storageKey?: string;
  /** Labels for the flick ring's faces and motions (by action id). */
  flickLabels?: Record<string, string>;
}

/** Everything the extras remember. */
export interface ExtrasSettings {
  hands: Record<Side, HandShape | null>;
  poses: SavedPose[];
  idlePose: string | null;
  faces: SavedFace[];
  face: Record<string, number>;
  props: PropState[];
  roll: number;
  filter: FilterState;
  gaze: GazeState;
  flick: FlickMode;
  flickSlots: FlickSlot[];
  liveStamps: boolean;
  danceMove: DanceMove | null;
}

export function defaultSettings(): ExtrasSettings {
  return {
    hands: { l: null, r: null },
    poses: [],
    idlePose: null,
    faces: [],
    face: {},
    props: [],
    roll: 0,
    filter: { id: 'none', strength: 0.8 },
    gaze: { mode: 'camera', x: 0, y: 0 },
    flick: 'off',
    flickSlots: DEFAULT_FLICK_SLOTS,
    liveStamps: false,
    danceMove: null,
  };
}

const clamp = (v: number, lo: number, hi: number) => (Number.isFinite(v) ? Math.min(hi, Math.max(lo, v)) : lo);

/** Validates stored settings (anything odd falls back to the default). */
export function parseSettings(raw: unknown): ExtrasSettings {
  const d = defaultSettings();
  const o = (raw && typeof raw === 'object' ? raw : {}) as Partial<Record<keyof ExtrasSettings, unknown>>;
  const shape = (v: unknown): HandShape | null => ((HAND_SHAPES as readonly string[]).includes(v as string) ? (v as HandShape) : null);
  const hands = (o.hands ?? {}) as Partial<Record<Side, unknown>>;
  d.hands = { l: shape(hands.l), r: shape(hands.r) };
  d.poses = (Array.isArray(o.poses) ? o.poses : []).map(cleanPose).filter((p): p is SavedPose => !!p).slice(0, 40);
  d.idlePose = typeof o.idlePose === 'string' && d.poses.some((p) => p.name === o.idlePose) ? o.idlePose : null;
  d.faces = (Array.isArray(o.faces) ? o.faces : [])
    .map((f) => (f && typeof f === 'object' && typeof (f as SavedFace).name === 'string' ? { name: (f as SavedFace).name.slice(0, 40), morphs: cleanMorphs((f as SavedFace).morphs) } : null))
    .filter((f): f is SavedFace => !!f)
    .slice(0, 40);
  d.face = cleanMorphs((o.face ?? {}) as Record<string, unknown>);
  d.props = (Array.isArray(o.props) ? o.props : [])
    .filter((p) => p && typeof p === 'object' && typeof (p as PropState).kind === 'string')
    .map((p) => clampProp(p as PropState))
    .slice(0, MAX_PROPS);
  d.roll = clamp(Number(o.roll ?? 0), -ROLL_LIMIT, ROLL_LIMIT);
  d.filter = normalizeFilter(o.filter as Partial<FilterState>);
  const g = (o.gaze ?? {}) as Partial<GazeState>;
  d.gaze = { mode: g.mode === 'ahead' || g.mode === 'custom' ? g.mode : 'camera', x: clamp(Number(g.x ?? 0), -1, 1), y: clamp(Number(g.y ?? 0), -1, 1) };
  d.flick = o.flick === 'hold' || o.flick === 'left' ? o.flick : 'off';
  if (Array.isArray(o.flickSlots) && o.flickSlots.length >= 4 && o.flickSlots.length <= 12) {
    const ok = o.flickSlots.every((s) => {
      const a = (s as FlickSlot)?.action;
      return a && ['face', 'motion', 'stamp'].includes(a.kind) && typeof a.id === 'string';
    });
    if (ok) d.flickSlots = o.flickSlots as FlickSlot[];
  }
  d.liveStamps = o.liveStamps === true;
  d.danceMove = (DANCE_MOVES as readonly string[]).includes(o.danceMove as string) ? (o.danceMove as DanceMove) : null;
  return d;
}

function defaultStorage(): ExtrasStorage | null {
  try {
    const ls = globalThis.localStorage;
    if (!ls) return null;
    return {
      get: (k) => {
        try {
          return ls.getItem(k);
        } catch {
          return null;
        }
      },
      set: (k, v) => {
        try {
          ls.setItem(k, v);
        } catch {
          /* full or blocked */
        }
      },
    };
  } catch {
    return null;
  }
}

export interface StudioExtras {
  readonly settings: Readonly<ExtrasSettings>;
  // S2
  setHand(side: Side | 'both', shape: HandShape | null): void;
  // S3
  editPose(on: boolean): void;
  readonly editing: boolean;
  /** Saves the pose on stage under `name` (replacing one with that name). */
  savePose(name: string): SavedPose | null;
  applyPose(name: string | null): void;
  deletePose(name: string): void;
  /** Holds a saved pose as her idle (remembered); null: the clips again. */
  setIdlePose(name: string | null): void;
  readonly activePose: string | null;
  // S5
  readonly morphs: string[];
  setMorph(name: string, weight: number): void;
  resetFace(): void;
  saveFace(name: string): SavedFace | null;
  applyFace(name: string | null): void;
  deleteFace(name: string): void;
  // S7
  loadProps(url?: string): Promise<void>;
  readonly propKinds: string[];
  addProp(kind: string): Promise<PropState | null>;
  updateProp(id: string, patch: Partial<Omit<PropState, 'id' | 'kind'>>): void;
  removeProp(id: string): void;
  clearProps(): void;
  selectProp(id: string | null): void;
  readonly selectedProp: PropState | null;
  // C9, C12, E3
  setRoll(deg: number): void;
  setFilter(f: Partial<FilterState>): void;
  setGaze(g: Partial<GazeState>): void;
  // B3
  setBackgroundImage(src: Blob | string | null): Promise<void>;
  readonly backgroundImage: boolean;
  // D6, D7
  setFlick(mode: FlickMode): void;
  setFlickSlots(slots: FlickSlot[]): void;
  setLiveStamps(on: boolean): void;
  /** Pops a stamp now (at client x/y, default: somewhere over her). */
  popStamp(id: string, x?: number, y?: number): void;
  fireFlick(slot: FlickSlot, x?: number, y?: number): void;
  // Dance
  setDanceMove(move: DanceMove | null): void;
  // P4
  snapshot(): Promise<Blob>;
  decorate(photo: Blob, opts?: DecoratorOptions): Promise<Blob | null>;
  /** Called after any change (with what changed: 'hands', 'pose', 'face', 'props', …). */
  onChange(cb: (what: string) => void): () => void;
  dispose(): void;
}

interface Bound {
  h: Humanoid;
  hands: Partial<Record<Side, HandBindingT | null>>;
  handActive: Record<Side, boolean>;
  leftSign: number;
  layer: PoseLayer;
  face: FaceMixer;
  danceW: number;
  dance: DanceLayer;
  /** Her feet centre at rest (world), where the props are placed from. */
  center: Vector3;
}

export function attachStudioExtras(stage: LucinaStage, options: StudioExtrasOptions = {}): StudioExtras {
  const storage = options.storage === undefined ? defaultStorage() : options.storage;
  const key = options.storageKey ?? 'secretary.studio2';
  let s: ExtrasSettings = defaultSettings();
  try {
    const raw = storage?.get(key);
    if (raw) s = parseSettings(JSON.parse(raw));
  } catch {
    s = defaultSettings();
  }
  const listeners = new Set<(what: string) => void>();
  let saveTimer: ReturnType<typeof setTimeout> | null = null;
  function changed(what: string): void {
    if (storage && !saveTimer) {
      saveTimer = setTimeout(() => {
        saveTimer = null;
        try {
          storage.set(key, JSON.stringify(s));
        } catch {
          /* ignore */
        }
      }, 250);
    }
    for (const cb of listeners) cb(what);
  }

  const bound = new WeakMap<Object3D, Bound>();
  const lib = new PropLibrary();
  const props = new PropSet(lib);
  props.onChange = () => {
    s.props = props.list;
    changed('props');
  };
  const editor = new PoseEditor();
  let editing = false;
  let editorModel: Object3D | null = null;
  let height = 1.65;
  let active: SavedPose | null = s.idlePose ? (s.poses.find((p) => p.name === s.idlePose) ?? null) : null;
  let danceT = 0;
  let stampT = 0;
  let bgTex: Texture | null = null;
  let bgPrev: string | null = null;
  const transient = new Map<string, { w: number; until: number }>();
  const canvasFilter = new WeakMap<HTMLCanvasElement, string>();
  let vignette: HTMLDivElement | null = null;
  let disposed = false;

  const cur = () => stage.three;
  const curCanvas = () => cur().renderer.domElement;

  function bind(model: Object3D, ctx: FrameContext): Bound {
    let b = bound.get(model);
    if (b) return b;
    const h = findHumanoid(model, ctx.bones as Record<string, string>);
    const rest = restPose(model);
    const hands: Bound['hands'] = {};
    let leftSign = 1;
    const center = new Vector3();
    withRestPose(model, rest, () => {
      (h.hips ?? model).getWorldPosition(center);
      hands.l = bindHand(h, 'l', rest.q);
      hands.r = bindHand(h, 'r', rest.q);
      const la = h.l.upperArm;
      const hd = h.head ?? h.neck;
      if (la && hd) {
        const x = la.getWorldPosition(new Vector3()).x - hd.getWorldPosition(new Vector3()).x;
        leftSign = Math.sign(x) || 1;
      }
    });
    const layer = new PoseLayer();
    layer.bind(model);
    const face = new FaceMixer();
    face.bind(model);
    b = { h, hands, handActive: { l: false, r: false }, leftSign, layer, face, danceW: 0, dance: new DanceLayer(), center };
    bound.set(model, b);
    return b;
  }

  function currentBound(): Bound | null {
    return bound.get(cur().model) ?? null;
  }

  // ---------- Pose hook (after the clips, before physics) ----------
  const poseHook = (ctx: FrameContext): void => {
    if (disposed || ctx.placeholder) return;
    const b = bind(ctx.model, ctx);
    const isCur = ctx.scene === cur().scene;
    const k = 1 - Math.exp(-ctx.dt / 0.08);

    // Pose (a saved/idle pose, or the one being edited).
    if (b.layer.pose !== active) b.layer.set(active, !editing);
    b.layer.apply(ctx.reducedMotion ? 1 : Math.max(k, 0.05));

    // Dance moves on top.
    const dancing = ctx.dancing && !!s.danceMove && !ctx.reducedMotion;
    b.danceW += ((dancing ? 1 : 0) - b.danceW) * (1 - Math.exp(-ctx.dt / 0.25));
    if (b.danceW < 0.002) b.danceW = 0;
    let danceHand: HandShape | null = null;
    if (b.danceW > 0 && s.danceMove) {
      if (isCur) danceT += ctx.dt;
      const f = sampleDance(s.danceMove, danceT);
      b.dance.apply(b.h, f, b.danceW, ctx.height, b.leftSign);
      danceHand = b.danceW > 0.5 ? f.hands : null;
    } else {
      b.dance.apply(b.h, null, 0, ctx.height, b.leftSign);
      if (isCur && !ctx.dancing) danceT = 0;
    }

    // Hands.
    for (const side of ['l', 'r'] as const) {
      const hb = b.hands[side];
      if (!hb) continue;
      const shape = danceHand ?? s.hands[side];
      if (shape) {
        applyHand(hb, HAND_PRESETS[shape], ctx.reducedMotion ? 1 : 1 - Math.exp(-ctx.dt / 0.07));
        b.handActive[side] = true;
      } else if (b.handActive[side]) {
        releaseHand(hb);
        b.handActive[side] = false;
      }
    }

    // Face: sliders plus short flick faces.
    const now = performance.now();
    const w: Record<string, number> = { ...s.face };
    for (const [name, t] of transient) {
      const left = (t.until - now) / 400;
      if (left <= 0) transient.delete(name);
      else w[name] = Math.max(w[name] ?? 0, t.w * Math.min(1, left));
    }
    b.face.weights = w;
    b.face.apply();
  };

  // ---------- Frame hook (after the camera, before the render) ----------
  const hook = (ctx: FrameContext): void => {
    if (disposed || ctx.placeholder) return;
    const b = bind(ctx.model, ctx);
    const isCur = ctx.scene === cur().scene;

    // Camera roll (after lookAt and the drift).
    if (s.roll) ctx.camera.rotateZ((s.roll * Math.PI) / 180);

    // Filters on every stage canvas.
    const css = cssFilter(s.filter);
    if (canvasFilter.get(ctx.canvas) !== css) {
      ctx.canvas.style.filter = css;
      canvasFilter.set(ctx.canvas, css);
    }

    if (!isCur) return;
    // Scene extras follow the current stage's scene.
    if (props.group.parent !== ctx.scene) ctx.scene.add(props.group);
    height = ctx.height;
    props.setFrame(new Vector3(b.center.x, ctx.floorY, b.center.z), ctx.height);
    if (editor.group.parent !== ctx.scene) ctx.scene.add(editor.group);
    if (editing) {
      if (editorModel !== ctx.model) {
        editorModel = ctx.model;
        editor.bind(b.h, ctx.height, b.leftSign);
      }
      editor.update(ctx.camera);
    }
    if (bgTex) {
      if (ctx.scene.background !== bgTex) ctx.scene.background = bgTex;
      coverFit(bgTex, ctx.canvas);
    }
    placeVignette(ctx.canvas);
    // Live stamps pop on their own while she dances.
    if (s.liveStamps && ctx.dancing) {
      stampT += ctx.dt;
      if (stampT > 1.9) {
        stampT = 0;
        const r = ctx.canvas.getBoundingClientRect();
        if (r.width > 40) {
          const id = STAMP_IDS[Math.floor(Math.random() * STAMP_IDS.length)]!;
          const side = Math.random() < 0.5 ? 0.18 : 0.82;
          void popStamp(id, r.left + r.width * (side + (Math.random() - 0.5) * 0.2), r.top + r.height * (0.2 + Math.random() * 0.55), { size: Math.min(110, r.width * 0.22), reduced: ctx.reducedMotion });
        }
      }
    } else stampT = 1.2;
  };
  stage.hooks.pose.add(poseHook);
  stage.hooks.frame.add(hook);

  function coverFit(t: Texture, canvas: HTMLCanvasElement): void {
    const img = t.image as { width?: number; height?: number } | undefined;
    if (!img?.width || !img.height) return;
    const ca = (canvas.clientWidth || canvas.width) / Math.max(1, canvas.clientHeight || canvas.height);
    const ia = img.width / img.height;
    if (ca > ia) {
      t.repeat.set(1, ia / ca);
      t.offset.set(0, (1 - ia / ca) / 2);
    } else {
      t.repeat.set(ca / ia, 1);
      t.offset.set((1 - ca / ia) / 2, 0);
    }
  }

  function placeVignette(canvas: HTMLCanvasElement): void {
    const bg = vignetteCss(s.filter);
    if (!bg) {
      if (vignette) vignette.style.display = 'none';
      return;
    }
    if (!vignette) {
      vignette = document.createElement('div');
      vignette.className = 'lucina-vignette';
      Object.assign(vignette.style, { position: 'fixed', pointerEvents: 'none', zIndex: '1' });
      document.body.append(vignette);
    }
    const r = canvas.getBoundingClientRect();
    Object.assign(vignette.style, { display: 'block', left: `${r.left}px`, top: `${r.top}px`, width: `${r.width}px`, height: `${r.height}px`, background: bg });
  }

  // ---------- Pointer routing (handles, props, flick ring) ----------
  const ray = new Raycaster();
  const ndc = new Vector2();
  type Mode =
    | { kind: 'handle'; id: number }
    | { kind: 'prop'; id: number; prop: string; offset: Vector3; plane: { y: number }; second?: { id: number; d0: number; a0: number; scale: number; rot: number; x: number; y: number }; firstXY: { x: number; y: number } }
    | { kind: 'ring'; id: number; ring: FlickRing; x: number; y: number };
  let mode: Mode | null = null;
  let pending: { id: number; x: number; y: number; timer: ReturnType<typeof setTimeout> } | null = null;
  let lockPrev: boolean | null = null;

  const lock = () => {
    if (lockPrev === null) lockPrev = stage.interactive;
    stage.setInteractive(false);
  };
  const unlock = () => {
    if (lockPrev) stage.setInteractive(true);
    lockPrev = null;
  };
  const rayAt = (x: number, y: number) => {
    const c = curCanvas();
    const r = c.getBoundingClientRect();
    ndc.set(((x - r.left) / Math.max(1, r.width)) * 2 - 1, -((y - r.top) / Math.max(1, r.height)) * 2 + 1);
    ray.setFromCamera(ndc, cur().camera as PerspectiveCamera);
    return ray;
  };
  const consume = (e: Event) => {
    e.stopPropagation();
    e.preventDefault();
  };

  function openRing(id: number, x: number, y: number): void {
    lock();
    const slots = s.flickSlots.map((sl) => ({ ...sl, label: options.flickLabels?.[sl.action.id] ?? sl.label }));
    mode = { kind: 'ring', id, ring: new FlickRing(x, y, slots), x, y };
  }

  const onDown = (e: PointerEvent): void => {
    if (disposed) return;
    const canvas = curCanvas();
    if (e.target !== canvas) return;
    if (mode?.kind === 'prop' && !mode.second && e.pointerType !== 'mouse') {
      // A second finger on a dragged prop: pinch scales, twist turns.
      const st = props.get(mode.prop);
      if (st) {
        const d = Math.hypot(e.clientX - mode.firstXY.x, e.clientY - mode.firstXY.y);
        mode.second = { id: e.pointerId, d0: Math.max(10, d), a0: Math.atan2(e.clientY - mode.firstXY.y, e.clientX - mode.firstXY.x), scale: st.scale, rot: st.rot, x: e.clientX, y: e.clientY };
      }
      consume(e);
      return;
    }
    if (mode) {
      consume(e);
      return;
    }
    const r = canvas.getBoundingClientRect();
    const x = e.clientX - r.left;
    const y = e.clientY - r.top;
    if (editing) {
      const id = editor.pick(cur().camera, x, y, r.width, r.height, e.pointerType === 'mouse' ? 18 : 30);
      if (id && editor.begin(id, rayAt(e.clientX, e.clientY).ray, cur().camera)) {
        consume(e);
        lock();
        mode = { kind: 'handle', id: e.pointerId };
        return;
      }
    }
    if (props.count) {
      const pid = props.pick(rayAt(e.clientX, e.clientY));
      if (pid) {
        consume(e);
        lock();
        props.select(pid);
        const st = props.get(pid)!;
        const planeY = props.origin.y + st.y;
        const hit = floorHit(planeY) ?? new Vector3(props.origin.x + st.x, planeY, props.origin.z + st.z);
        mode = { kind: 'prop', id: e.pointerId, prop: pid, offset: new Vector3(props.origin.x + st.x - hit.x, 0, props.origin.z + st.z - hit.z), plane: { y: planeY }, firstXY: { x: e.clientX, y: e.clientY } };
        changed('select');
        return;
      }
      if (props.selected) {
        props.select(null);
        changed('select');
      }
    }
    if (s.flick === 'left' && x < r.width / 2) {
      consume(e);
      openRing(e.pointerId, e.clientX, e.clientY);
      return;
    }
    if (s.flick === 'hold' && e.isPrimary) {
      if (pending) clearTimeout(pending.timer);
      const id = e.pointerId;
      pending = {
        id,
        x: e.clientX,
        y: e.clientY,
        timer: setTimeout(() => {
          const p = pending;
          pending = null;
          if (p && !mode && !disposed) openRing(p.id, p.x, p.y);
        }, 320),
      };
    }
  };

  function floorHit(y: number): Vector3 | null {
    const p = new Vector3();
    const o = ray.ray.origin;
    const d = ray.ray.direction;
    if (Math.abs(d.y) < 1e-5) return null;
    const t = (y - o.y) / d.y;
    if (t < 0) return null;
    return p.copy(o).addScaledVector(d, t);
  }

  const onMove = (e: PointerEvent): void => {
    if (pending && pending.id === e.pointerId && Math.hypot(e.clientX - pending.x, e.clientY - pending.y) > 10) {
      clearTimeout(pending.timer);
      pending = null;
    }
    if (!mode) return;
    if (mode.kind === 'prop' && mode.second) {
      const m = mode;
      const sec = m.second!;
      if (e.pointerId === m.id) m.firstXY = { x: e.clientX, y: e.clientY };
      else if (e.pointerId === sec.id) {
        sec.x = e.clientX;
        sec.y = e.clientY;
      } else return;
      consume(e);
      const d = Math.hypot(sec.x - m.firstXY.x, sec.y - m.firstXY.y);
      const a = Math.atan2(sec.y - m.firstXY.y, sec.x - m.firstXY.x);
      props.update(m.prop, { scale: sec.scale * (d / sec.d0), rot: sec.rot - ((a - sec.a0) * 180) / Math.PI });
      return;
    }
    if (e.pointerId !== mode.id) return;
    consume(e);
    if (mode.kind === 'handle') {
      editor.move(rayAt(e.clientX, e.clientY).ray);
      const name = active?.name ?? '';
      active = editor.capture(name);
      const b = currentBound();
      if (b) b.layer.set(active, false);
    } else if (mode.kind === 'prop') {
      rayAt(e.clientX, e.clientY);
      const hit = floorHit(mode.plane.y);
      if (hit) {
        const p = hit.add(mode.offset);
        props.update(mode.prop, { x: p.x - props.origin.x, z: p.z - props.origin.z });
      }
    } else mode.ring.track(e.clientX - mode.x, e.clientY - mode.y);
  };

  const onUp = (e: PointerEvent): void => {
    if (pending && pending.id === e.pointerId) {
      clearTimeout(pending.timer);
      pending = null;
    }
    if (!mode) return;
    if (mode.kind === 'prop' && mode.second && (e.pointerId === mode.second.id || e.pointerId === mode.id)) {
      consume(e);
      // Back to a one-finger drag with the remaining finger (or end).
      if (e.pointerId === mode.second.id) {
        mode.second = undefined;
        return;
      }
    }
    if (e.pointerId !== mode.id) return;
    consume(e);
    const m = mode;
    mode = null;
    if (m.kind === 'handle') {
      editor.end();
      changed('pose');
    } else if (m.kind === 'ring') {
      const i = m.ring.selected;
      m.ring.close();
      const slot = i >= 0 ? s.flickSlots[i] : undefined;
      if (slot) fireFlick(slot, m.x, m.y);
    }
    unlock();
  };
  const onCancel = (e: PointerEvent): void => {
    if (pending && pending.id === e.pointerId) {
      clearTimeout(pending.timer);
      pending = null;
    }
    if (!mode || e.pointerId !== mode.id) return;
    if (mode.kind === 'ring') mode.ring.close();
    if (mode.kind === 'handle') editor.end();
    mode = null;
    unlock();
  };
  const doc = typeof document !== 'undefined' ? document : null;
  doc?.addEventListener('pointerdown', onDown, true);
  doc?.addEventListener('pointermove', onMove, true);
  doc?.addEventListener('pointerup', onUp, true);
  doc?.addEventListener('pointercancel', onCancel, true);

  // ---------- Actions ----------
  function fireFlick(slot: FlickSlot, x?: number, y?: number): void {
    const a = slot.action;
    if (a.kind === 'stamp') {
      api.popStamp(a.id, x, y);
    } else if (a.kind === 'face') {
      const model = cur().model;
      if (listMorphs(model).includes(a.id)) transient.set(a.id, { w: 1, until: performance.now() + 2600 });
      else {
        stage.setExpression(a.id as never, 1);
        setTimeout(() => stage.setExpression(null), 2600);
      }
    } else if (stage.info.clips.some((c) => c.name === a.id)) void stage.play(a.id).catch(() => {});
    else stage.gesture(a.id);
    changed('flick-fired');
  }

  function poseNamed(name: string): SavedPose | null {
    return s.poses.find((p) => p.name === name) ?? null;
  }

  const api: StudioExtras = {
    get settings() {
      return s;
    },
    setHand(side, shape) {
      const v = shape && (HAND_SHAPES as readonly string[]).includes(shape) ? shape : null;
      if (side === 'both' || side === 'l') s.hands.l = v;
      if (side === 'both' || side === 'r') s.hands.r = v;
      changed('hands');
    },
    editPose(on) {
      editing = !!on;
      const b = currentBound();
      if (editing && b) {
        editor.touched.clear();
        editor.bind(b.h, height, b.leftSign);
        editorModel = stage.three.model;
        // Start from what she is doing now (frozen), or the pose already held.
        if (!active) {
          active = editor.capture('');
          b.layer.set(active, false);
        } else if (active.bones[b.h.head?.name ?? '']) editor.touched.add('head');
      }
      editor.group.visible = editing;
      if (!editing && mode?.kind === 'handle') {
        editor.end();
        mode = null;
        unlock();
      }
      changed('editing');
    },
    get editing() {
      return editing;
    },
    savePose(name) {
      const n = name.trim().slice(0, 40);
      if (!n || !active) return null;
      const pose: SavedPose = { ...active, name: n };
      s.poses = [...s.poses.filter((p) => p.name !== n), pose];
      active = pose;
      changed('poses');
      return pose;
    },
    applyPose(name) {
      active = name ? poseNamed(name) : null;
      if (!active) {
        const b = currentBound();
        b?.layer.clear();
      }
      changed('pose');
    },
    deletePose(name) {
      s.poses = s.poses.filter((p) => p.name !== name);
      if (s.idlePose === name) s.idlePose = null;
      changed('poses');
    },
    setIdlePose(name) {
      s.idlePose = name && poseNamed(name) ? name : null;
      api.applyPose(s.idlePose);
      changed('poses');
    },
    get activePose() {
      return active?.name || (active ? '' : null);
    },
    get morphs() {
      return listMorphs(cur().model);
    },
    setMorph(name, weight) {
      const w = clamp(weight, 0, 1);
      const next = { ...s.face };
      if (w > 0.001) next[name] = w;
      else delete next[name];
      s.face = next;
      changed('face');
    },
    resetFace() {
      s.face = {};
      changed('face');
    },
    saveFace(name) {
      const n = name.trim().slice(0, 40);
      if (!n) return null;
      const face: SavedFace = { name: n, morphs: { ...s.face } };
      s.faces = [...s.faces.filter((f) => f.name !== n), face];
      changed('faces');
      return face;
    },
    applyFace(name) {
      const f = name ? s.faces.find((x) => x.name === name) : null;
      s.face = f ? { ...f.morphs } : {};
      changed('face');
    },
    deleteFace(name) {
      s.faces = s.faces.filter((f) => f.name !== name);
      changed('faces');
    },
    async loadProps(url) {
      const u = url ?? options.propsUrl;
      if (!u) throw new Error('No props URL');
      await lib.load(u, options.fetchInit);
      if (!props.count && s.props.length) props.load(s.props);
      changed('props-loaded');
    },
    get propKinds() {
      return lib.kinds;
    },
    async addProp(kind) {
      if (!lib.loaded) await api.loadProps();
      return props.add(kind);
    },
    updateProp(id, patch) {
      props.update(id, patch);
    },
    removeProp(id) {
      props.remove(id);
    },
    clearProps() {
      props.clear();
    },
    selectProp(id) {
      props.select(id);
      changed('select');
    },
    get selectedProp() {
      return props.selected ? props.get(props.selected) : null;
    },
    setRoll(deg) {
      s.roll = clamp(deg, -ROLL_LIMIT, ROLL_LIMIT);
      changed('roll');
    },
    setFilter(f) {
      s.filter = normalizeFilter({ ...s.filter, ...f });
      changed('filter');
    },
    setGaze(g) {
      s.gaze = { ...s.gaze, ...g, x: clamp(Number(g.x ?? s.gaze.x), -1, 1), y: clamp(Number(g.y ?? s.gaze.y), -1, 1) };
      applyGaze();
      changed('gaze');
    },
    async setBackgroundImage(src) {
      if (!src) {
        const t = bgTex;
        bgTex = null;
        const sc = cur().scene;
        if (t && sc.background === t) sc.background = null;
        t?.dispose();
        if (bgPrev !== null) stage.setBackground(bgPrev);
        bgPrev = null;
        changed('background');
        return;
      }
      const img = await loadAnyImage(src);
      const t = new Texture(img);
      t.colorSpace = SRGBColorSpace;
      t.needsUpdate = true;
      bgTex?.dispose();
      bgTex = t;
      if (bgPrev === null) bgPrev = stage.background === 'transparent' ? 'default' : stage.background;
      stage.setBackground('transparent');
      changed('background');
    },
    get backgroundImage() {
      return !!bgTex;
    },
    setFlick(m) {
      s.flick = m === 'hold' || m === 'left' ? m : 'off';
      changed('flick');
    },
    setFlickSlots(slots) {
      s.flickSlots = parseSettings({ flickSlots: slots }).flickSlots;
      changed('flick');
    },
    setLiveStamps(on) {
      s.liveStamps = !!on;
      changed('liveStamps');
    },
    popStamp(id, x, y) {
      const c = curCanvas();
      const r = c.getBoundingClientRect();
      const px = x ?? r.left + r.width * (0.3 + Math.random() * 0.4);
      const py = y ?? r.top + r.height * (0.25 + Math.random() * 0.3);
      const reduced = typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
      void popStamp(id, px, py, { size: Math.max(64, Math.min(120, r.width * 0.24)), reduced });
    },
    fireFlick,
    setDanceMove(move) {
      s.danceMove = move && (DANCE_MOVES as readonly string[]).includes(move) ? move : null;
      // The clip dance would fight the procedural one.
      if (s.danceMove && stage.dancing && stage.clip === 'dance_loop') stage.stopClip(0.3);
      changed('dance');
    },
    async snapshot() {
      const hv = editor.group.visible;
      editor.group.visible = false;
      props.select(null);
      try {
        return await filterBlob(await stage.snapshot(), s.filter);
      } finally {
        editor.group.visible = hv;
      }
    },
    decorate(photo, opts) {
      return openPhotoDecorator(document.body, photo, opts).result;
    },
    onChange(cb) {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      stage.hooks.pose.delete(poseHook);
      stage.hooks.frame.delete(hook);
      stage.hooks.gaze = null;
      doc?.removeEventListener('pointerdown', onDown, true);
      doc?.removeEventListener('pointermove', onMove, true);
      doc?.removeEventListener('pointerup', onUp, true);
      doc?.removeEventListener('pointercancel', onCancel, true);
      if (pending) clearTimeout(pending.timer);
      if (mode?.kind === 'ring') mode.ring.close();
      if (saveTimer) {
        clearTimeout(saveTimer);
        saveTimer = null;
        try {
          storage?.set(key, JSON.stringify(s));
        } catch {
          /* ignore */
        }
      }
      const b = currentBound();
      b?.layer.clear();
      for (const side of ['l', 'r'] as const) if (b?.hands[side] && b.handActive[side]) releaseHand(b.hands[side]!);
      b?.face.release();
      try {
        curCanvas().style.filter = '';
      } catch {
        /* disposed stage */
      }
      vignette?.remove();
      props.dispose();
      lib.dispose();
      editor.dispose();
      bgTex?.dispose();
      listeners.clear();
    },
  };

  function applyGaze(): void {
    const g = s.gaze;
    stage.hooks.gaze = g.mode === 'camera' ? null : g.mode === 'ahead' ? { yaw: 0, pitch: 0 } : { yaw: g.x * GAZE_RANGE.yaw, pitch: -g.y * GAZE_RANGE.pitch };
  }
  applyGaze();
  if (s.props.length && options.propsUrl) void api.loadProps().catch(() => {});
  return api;
}

function loadAnyImage(src: Blob | string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    let url: string | null = null;
    img.onload = () => {
      if (url) URL.revokeObjectURL(url);
      resolve(img);
    };
    img.onerror = () => {
      if (url) URL.revokeObjectURL(url);
      reject(new Error('Background image could not be read'));
    };
    if (typeof src === 'string') img.src = src;
    else img.src = url = URL.createObjectURL(src);
  });
}
