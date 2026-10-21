/**
 * createLucinaStage: renderer, scene, lights, model (GLB or placeholder), procedural animation,
 * WebAudio playback with lip-sync, look-at, tap detection, visibility-aware render loop.
 */
import {
  ACESFilmicToneMapping,
  AgXToneMapping,
  Box3,
  Color,
  DataTexture,
  DirectionalLight,
  Group,
  HemisphereLight,
  Mesh,
  MeshBasicMaterial,
  NeutralToneMapping,
  Object3D,
  PerspectiveCamera,
  PlaneGeometry,
  PMREMGenerator,
  Raycaster,
  RGBAFormat,
  Scene,
  SRGBColorSpace,
  Vector2,
  Vector3,
  WebGLRenderer,
  type Material,
  type Texture,
  type ToneMapping,
} from 'three';
import { RoomEnvironment } from 'three/addons/environments/RoomEnvironment.js';
import { Animator } from './animator';
import { AnalyserLipSync, LipSyncProcessor, type LipSyncFrame } from './lipsync';
import { fixMaterials, hideNodes, loadManifest, loadModel } from './loader';
import { normalizeManifest, type CameraFraming, type NormalizedManifest } from './manifest';
import { MUTE_STORAGE_KEY, readStoredMuted, writeMuted } from './mute';
import { createPlaceholder } from './placeholder';
import { Rig, type RigReport } from './rig';
import { ClipPlayer, loadClipLibrary, type ClipInfo, type ClipLibrary, type PlayOptions } from './clips';
import { damp } from './spring';
import { StageStateMachine } from './stateMachine';
import { StageEnvironment, type EnvironmentOptions } from './scene/environment';
import { MotionLayer, clipNames } from './scene/motion';
import type { StageQuality, StageQualitySetting, StageTheme } from './scene/quality';
import { FrameStats } from './stats';
import { clamp, type Expression, type Framing, type StageState } from './types';

export interface LucinaStageOptions {
  /** GLB URL. Production: `/v1/assets/lucina.glb`. Omit to use the procedural placeholder. */
  modelUrl?: string;
  /** Manifest URL (morph/bone names, camera). Optional. */
  manifestUrl?: string;
  /** Applied to the model and manifest requests: `{ credentials: 'include' }` or an Authorization header. */
  fetchInit?: RequestInit;
  framing?: Framing;
  /** Only blinks and lip-sync. Defaults to the `prefers-reduced-motion` media query (and follows it). */
  reducedMotion?: boolean;
  /** Force the placeholder even when `modelUrl` is set. */
  placeholder?: boolean;
  /** Use the placeholder when the model fails to load (default true). The error goes to `onError`. */
  fallbackToPlaceholder?: boolean;
  onError?: (error: unknown) => void;
  /** Device pixel ratio cap (default 2). */
  maxPixelRatio?: number;
  toneMapping?: 'aces' | 'agx' | 'neutral';
  exposure?: number;
  /** localStorage key for the mute preference; `null` disables persistence. */
  muteStorageKey?: string | null;
  /** Initial mute state when nothing is stored. */
  muted?: boolean;
  /** Observe the canvas size and resize automatically (default true). */
  autoResize?: boolean;
  antialias?: boolean;
  /** Abort the model download. */
  signal?: AbortSignal;
  /** Hide meshes using these materials (default: the MMD expression icons and the sword). */
  hideMaterials?: string[];
  /** Model morphs held at a fixed weight, by model morph name. */
  fixedMorphs?: Record<string, number>;
  /**
   * Relaxes an A/T-pose: upper arms are rotated down to `down` degrees below horizontal,
   * slightly forward, with a soft elbow bend. `false` keeps the authored pose.
   */
  armPose?: { down?: number; forward?: number; elbow?: number } | false;
  /**
   * The stage set: nebula sky, holographic floor ring, particles, neon rim light, camera drift,
   * bloom on the high tier (src/scene/). Off by default: transparent canvas, the host paints it.
   */
  environment?: boolean | EnvironmentOptions;
  /**
   * Body animation clips (lucina-anims.glb, same skeleton as the model, no mesh), e.g.
   * `/v1/assets/lucina-anims.glb`. With clips, an idle clip always plays under the procedural
   * gaze/blink/lip-sync, so she never stands in the rest pose. Without (or if loading fails),
   * the procedural idle with relaxed arms is used.
   */
  clipsUrl?: string;
  /** clips.json for the clip file (loop flags, tags). Optional. */
  clipsMetaUrl?: string;
  /** Clip pools per state (defaults: idle_breathe/idle_shift, listen_lean, think_chin, talk_gesture_*). */
  stateClips?: Partial<Record<StageState, string[]>>;
}

export const DEFAULT_HIDE_MATERIALS = ['ExpIcons', 'Falchion'];
/** Applied only if the model has them: "sword off" keeps the Falchion out of the shot. */
export const DEFAULT_FIXED_MORPHS: Record<string, number> = { 'ファルシオンOFF': 1 };

export interface TapEvent {
  /** Name of the mesh that was hit. */
  object: string;
  /** Hit point in world space. */
  point: { x: number; y: number; z: number };
  clientX: number;
  clientY: number;
}

export interface SpeakOptions {
  /** Called when playback ends or is stopped. */
  onEnd?: () => void;
}

export interface StageInfo {
  placeholder: boolean;
  /** Clips bound to the model ([] when no clip file is loaded). */
  clips: ClipInfo[];
  extensions: string[];
  rig: RigReport;
  hidden: string[];
}

export interface LucinaStage {
  readonly state: StageState;
  setState(state: StageState): void;
  onStateChange(cb: (next: StageState, prev: StageState) => void): () => void;
  /** Plays the audio through WebAudio and drives the lip-sync. Resolves when playback ends. */
  speak(audio: Blob | ArrayBuffer | ArrayBufferView, opts?: SpeakOptions): Promise<void>;
  stopSpeaking(): void;
  readonly speaking: boolean;
  setMuted(muted: boolean): void;
  isMuted(): boolean;
  setExpression(expression: Expression | null, weight?: number): void;
  /** Tap/click on Lucina herself (raycast on her meshes). Returns an unsubscribe function. */
  onTap(cb: (e: TapEvent) => void): () => void;
  lookAtPointer(enabled: boolean): void;
  /** Small acknowledging nod, e.g. when user speech is detected while listening. */
  nod(strength?: number): void;
  setFraming(framing: Framing): void;
  setReducedMotion(reduced: boolean | null): void;
  /** Creates/resumes the AudioContext; call from a user gesture on iOS. */
  unlockAudio(): Promise<void>;
  resize(): void;
  /**
   * Plays a body clip by name (see `info.clips`). One-shots resolve when they end and return to the
   * state's idle pool; loops resolve at once and play until the next play/stopClip/state change.
   * Rejects for unknown names or when no clip file is loaded.
   */
  play(clip: string, opts?: PlayOptions): Promise<void>;
  /** Ends the current clip and returns to the state's pool (the idle never shows the rest pose). */
  stopClip(fade?: number): void;
  /** Name of the clip playing now, or null without clips. */
  readonly clip: string | null;
  /** Replace the clip pools per state, e.g. `{ speaking: ['talk_gesture_2'] }`. */
  setStateClips(map: Partial<Record<StageState, string[]>>): void;
  dispose(): void;
  readonly stats: FrameStats;
  readonly info: StageInfo;
  /** Procedural gesture by name (nod, tilt, happy, shy, surprised, greet, think); works without a clip file. */
  gesture(name: string): boolean;
  /** Names `gesture` accepts. */
  readonly gestures: string[];
  /**
   * Tap reaction: a reaction clip when the clip file has one, else a procedural gesture; with the
   * environment also a sparkle burst at `at` (world space). Returns what played.
   */
  react(at?: { x: number; y: number; z: number }): string;
  /** Environment quality tier; no-op without the environment. */
  setQuality(q: StageQualitySetting): void;
  /** Current environment tier, null without the environment. */
  readonly quality: StageQuality | null;
  /** Environment palette; no-op without the environment. */
  setTheme(theme: StageTheme): void;
  /** Escape hatches for debugging. */
  readonly three: { renderer: WebGLRenderer; scene: Scene; camera: PerspectiveCamera; model: Object3D };
}

type AudioCtor = typeof AudioContext;

function audioContextCtor(): AudioCtor | null {
  const g = globalThis as unknown as { AudioContext?: AudioCtor; webkitAudioContext?: AudioCtor };
  return g.AudioContext ?? g.webkitAudioContext ?? null;
}

function decode(ctx: AudioContext, data: ArrayBuffer): Promise<AudioBuffer> {
  return new Promise((resolve, reject) => {
    // Old WebKit only supports the callback form; newer ones return a promise too.
    const p = ctx.decodeAudioData(data, resolve, reject) as Promise<AudioBuffer> | undefined;
    p?.then(resolve, reject);
  });
}

function shadowTexture(): DataTexture {
  const n = 64;
  const data = new Uint8Array(n * n * 4);
  for (let y = 0; y < n; y++) {
    for (let x = 0; x < n; x++) {
      const dx = (x + 0.5) / n - 0.5;
      const dy = (y + 0.5) / n - 0.5;
      const r = Math.sqrt(dx * dx + dy * dy) * 2;
      const a = Math.pow(clamp(1 - r, 0, 1), 1.8);
      const i = (y * n + x) * 4;
      data[i] = data[i + 1] = data[i + 2] = 255;
      data[i + 3] = Math.round(a * 255);
    }
  }
  const t = new DataTexture(data, n, n, RGBAFormat);
  t.needsUpdate = true;
  return t;
}

function prefersReducedMotion(): MediaQueryList | null {
  try {
    return typeof matchMedia === 'function' ? matchMedia('(prefers-reduced-motion: reduce)') : null;
  } catch {
    return null;
  }
}

const DEG = Math.PI / 180;

/**
 * Lowers A/T-pose arms. Uses the manifest's recommended idle rotation when it names the upper-arm
 * node, otherwise the measured rest direction of each upper arm, aiming `down` degrees below horizontal.
 */
function relaxArms(rig: Rig, cfg: { down?: number; forward?: number; elbow?: number }, manifest: NormalizedManifest): void {
  const target = (cfg.down ?? 80) * DEG;
  const forward = (cfg.forward ?? 6) * DEG;
  const elbow = (cfg.elbow ?? 12) * DEG;
  for (const [upper, lower] of [
    ['upper_arm_l', 'lower_arm_l'],
    ['upper_arm_r', 'lower_arm_r'],
  ] as const) {
    const a = rig.bonePosition(upper);
    const b = rig.bonePosition(lower);
    if (!a || !b) continue;
    const dir = b.sub(a);
    const side = Math.sign(dir.x) || 1;
    const nodeName = rig.report.bones[upper];
    const rec = nodeName ? manifest.idle.rotations[nodeName] : undefined;
    let roll: number;
    if (rec && Math.abs(rec.axis[2]) > 0.9) {
      roll = rec.angleDeg * DEG * Math.sign(rec.axis[2]);
      // The recommendation can leave the arms out in an A: lower them the rest of the way.
      const rest = Math.atan2(-dir.y, Math.abs(dir.x));
      const need = clamp(target - rest, 0, 80 * DEG);
      if (need > Math.abs(roll)) roll = -side * need;
    } else {
      const current = Math.atan2(-dir.y, Math.abs(dir.x)); // angle below horizontal
      const delta = clamp(target - current, 0, 80 * DEG);
      if (delta < 2 * DEG) continue;
      // Rotating about +Z by a negative angle swings a +X arm downward (and vice versa).
      roll = -side * delta;
    }
    rig.setOffset(upper, { x: -forward, y: 0, z: roll });
    // Forearms follow a little further in, so the hands rest by the hips instead of out in an A.
    rig.setOffset(lower, { x: -elbow, y: 0, z: -side * 8 * DEG });
  }
}

const TONE: Record<NonNullable<LucinaStageOptions['toneMapping']>, ToneMapping> = {
  aces: ACESFilmicToneMapping,
  agx: AgXToneMapping,
  neutral: NeutralToneMapping,
};

export async function createLucinaStage(canvas: HTMLCanvasElement, options: LucinaStageOptions = {}): Promise<LucinaStage> {
  // ---------- Renderer / scene ----------
  const renderer = new WebGLRenderer({
    canvas,
    alpha: true,
    antialias: options.antialias ?? true,
    premultipliedAlpha: true,
    powerPreference: 'high-performance',
  });
  renderer.setClearColor(0x000000, 0);
  renderer.outputColorSpace = SRGBColorSpace;
  renderer.toneMapping = TONE[options.toneMapping ?? 'aces'];
  renderer.toneMappingExposure = options.exposure ?? 0.9;
  const maxDpr = options.maxPixelRatio ?? 2;
  let env: StageEnvironment | null = null;

  const scene = new Scene();
  const camera = new PerspectiveCamera(30, 1, 0.01, 100);

  const pmrem = new PMREMGenerator(renderer);
  const envTex = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;
  pmrem.dispose();
  scene.environment = envTex;
  scene.environmentIntensity = 0.25;

  const hemi = new HemisphereLight(0xfff3ec, 0x5d6488, 0.85);
  const key = new DirectionalLight(0xfff4ea, 1.7);
  const rim = new DirectionalLight(0xc8dcff, 2.0);
  const rim2 = new DirectionalLight(0xffe2f0, 0.9);
  const lightTarget = new Object3D();
  key.target = rim.target = rim2.target = lightTarget;
  scene.add(hemi, key, rim, rim2, lightTarget);

  // ---------- Model ----------
  let manifest: NormalizedManifest = normalizeManifest(null);
  let model: Object3D;
  let placeholder = false;
  let extensions: string[] = [];
  let clipLib: ClipLibrary | null = null;
  const usePlaceholder = options.placeholder || !options.modelUrl;
  const clipsPromise: Promise<ClipLibrary | null> =
    !usePlaceholder && options.clipsUrl
      ? loadClipLibrary(options.clipsUrl, options.clipsMetaUrl, options).catch((e: unknown) => {
          console.warn('[lucina-stage] clips not loaded, using the procedural idle:', e);
          return null;
        })
      : Promise.resolve(null);
  if (!usePlaceholder) {
    try {
      const [m, loaded] = await Promise.all([
        loadManifest(options.manifestUrl, options).catch((e: unknown) => {
          // A missing manifest only costs the custom names; defaults still apply.
          console.warn('[lucina-stage] manifest not loaded, using default names:', e);
          return normalizeManifest(null);
        }),
        loadModel(options.modelUrl!, options),
      ]);
      manifest = m;
      model = loaded.gltf.scene;
      extensions = loaded.extensions;
      clipLib = await clipsPromise;
    } catch (e) {
      if (options.fallbackToPlaceholder === false) {
        renderer.dispose();
        throw e;
      }
      console.warn('[lucina-stage] model failed to load, using the placeholder:', e);
      options.onError?.(e);
      model = createPlaceholder().root;
      placeholder = true;
    }
  } else {
    model = createPlaceholder().root;
    placeholder = true;
  }

  const container = new Group();
  container.name = 'LucinaContainer';
  container.add(model);
  scene.add(container);
  const hidden = placeholder ? [] : hideNodes(model, manifest.hide);
  if (!placeholder) {
    const hideMats = new Set([...(options.hideMaterials ?? DEFAULT_HIDE_MATERIALS), ...manifest.hideMaterials]);
    model.traverse((o) => {
      const me = o as Mesh;
      if (!me.isMesh) return;
      const mats = Array.isArray(me.material) ? me.material : [me.material];
      if (mats.some((mt) => mt && hideMats.has(mt.name))) {
        me.visible = false;
        hidden.push(`${me.name} (${mats.map((mt) => mt?.name).join(',')})`);
      }
    });
    fixMaterials(model);
  }

  let rig = new Rig(container, manifest);
  // Face the camera (+Z). Use the manifest, else detect from eyes vs head.
  if (!placeholder) {
    let flip = manifest.facing === '-z';
    if (!manifest.facing) {
      const head = rig.bonePosition('head');
      const eye = rig.bonePosition('eye_l') ?? rig.bonePosition('eye_r') ?? rig.bonePosition('eyes');
      if (head && eye && eye.z < head.z - 1e-4) flip = true;
    }
    if (flip) {
      model.rotation.y += Math.PI;
      rig = new Rig(container, manifest); // rest poses relative to the new orientation
    }
  }
  rig.setFixedMorphs({ ...DEFAULT_FIXED_MORPHS, ...manifest.idle.morphs, ...options.fixedMorphs });
  if (options.armPose !== false) relaxArms(rig, options.armPose ?? {}, manifest);
  let player: ClipPlayer | null = null;
  if (clipLib && !placeholder) {
    player = new ClipPlayer(model, clipLib, { stateClips: options.stateClips });
    if (!player.ready) {
      console.warn('[lucina-stage] no clip in the clip file matches this model; using the procedural idle');
      player.dispose();
      player = null;
    }
  }
  const frame0 = new Animator().update(0, 'idle', { a: 0, i: 0, u: 0, e: 0, o: 0 });
  if (player) {
    rig.restoreBones();
    player.update(0);
    rig.apply(frame0, { driven: player.drivenNodes });
  } else rig.apply(frame0);
  if (rig.report.missingBones.length || rig.report.missingMorphs.length) {
    console.info('[lucina-stage] unbound bones:', rig.report.missingBones, 'morphs:', rig.report.missingMorphs);
  }

  container.updateMatrixWorld(true);
  const bbox = new Box3().setFromObject(container);
  const size = bbox.getSize(new Vector3());
  const height = size.y || 1.6;
  const headPos =
    rig.bonePosition('head') ?? new Vector3((bbox.min.x + bbox.max.x) / 2, bbox.max.y - height * 0.12, (bbox.min.z + bbox.max.z) / 2);
  const eyePos = (() => {
    const l = rig.bonePosition('eye_l');
    const r = rig.bonePosition('eye_r');
    if (l && r) return l.add(r).multiplyScalar(0.5);
    return l ?? r ?? rig.bonePosition('eyes') ?? headPos.clone().add(new Vector3(0, height * 0.06, height * 0.05));
  })();
  const faceCenter = new Vector3(headPos.x, (headPos.y + eyePos.y) / 2 + height * 0.02, Math.max(headPos.z, eyePos.z));

  // Lights relative to the face, scaled to the model.
  lightTarget.position.copy(faceCenter);
  key.position.copy(faceCenter).add(new Vector3(0.9, 0.85, 1.7).multiplyScalar(height));
  rim.position.copy(faceCenter).add(new Vector3(-1.3, 0.7, -1.4).multiplyScalar(height));
  rim2.position.copy(faceCenter).add(new Vector3(1.2, 0.3, -1.3).multiplyScalar(height));

  // Contact shadow.
  const shadowTex = shadowTexture();
  const shadowSize = Math.max(size.x, size.z, height * 0.3) * 1.25;
  const shadow = new Mesh(
    new PlaneGeometry(shadowSize, shadowSize * 0.75),
    new MeshBasicMaterial({ map: shadowTex, color: new Color(0x1b1830), transparent: true, opacity: 0.5, depthWrite: false, toneMapped: false }),
  );
  shadow.name = 'ContactShadow';
  shadow.rotation.x = -Math.PI / 2;
  shadow.position.set((bbox.min.x + bbox.max.x) / 2, bbox.min.y + height * 0.001, (bbox.min.z + bbox.max.z) / 2);
  shadow.renderOrder = -1;
  scene.add(shadow);

  // Procedural body layer (arm sway without clips, gestures always).
  const leftArm = rig.bonePosition('upper_arm_l');
  const motion = new MotionLayer(leftArm ? Math.sign(leftArm.x - headPos.x) || 1 : 1);

  // ---------- Camera framing ----------
  let framing: Framing = options.framing ?? 'bust';
  const camGoal = { pos: new Vector3(), target: new Vector3(), fov: 30 };
  const camNow = { pos: new Vector3(), target: new Vector3(), fov: 30, init: false };

  function manifestFraming(f: Framing): CameraFraming | undefined {
    const c = manifest.camera[f];
    if (!c?.position || !c.target) return undefined;
    // Only trust it if it actually looks at her: bust near the face, full near the body centre.
    const ty = c.target[1];
    const ok = f === 'bust' ? Math.abs(ty - eyePos.y) < height * 0.12 : ty > bbox.min.y && ty < headPos.y;
    if (!ok) {
      if (!warnedFraming) console.info(`[lucina-stage] manifest ${f} camera ignored (target off the subject)`);
      warnedFraming = true;
      return undefined;
    }
    return c;
  }
  let warnedFraming = false;

  function computeFraming(): void {
    const aspect = camera.aspect || 1;
    const mf = placeholder ? undefined : manifestFraming(framing);
    if (mf?.position && mf.target) {
      model.updateMatrixWorld(true);
      camGoal.pos.fromArray(mf.position).applyMatrix4(model.matrixWorld);
      camGoal.target.fromArray(mf.target).applyMatrix4(model.matrixWorld);
      camGoal.fov = mf.fov ?? 30;
      // Keep the manifest's framing; only very narrow canvases back off a little.
      if (aspect < 0.6) {
        const dir = camGoal.pos.clone().sub(camGoal.target);
        camGoal.pos.copy(camGoal.target).add(dir.multiplyScalar(Math.min(0.6 / aspect, 1.3)));
      }
      return;
    }
    const fov = framing === 'bust' ? 28 : 30;
    const t = Math.tan(((fov / 2) * Math.PI) / 180);
    const headTop = Math.min(bbox.max.y, headPos.y + height * 0.16);
    let top: number;
    let bottom: number;
    let halfW: number;
    if (framing === 'bust') {
      // Head and shoulders: crown to upper chest; on narrow canvases the shoulders may crop a little.
      top = headTop + height * 0.045;
      bottom = headPos.y - height * 0.17;
      halfW = height * 0.105;
    } else {
      top = bbox.max.y + height * 0.03;
      bottom = bbox.min.y - height * 0.02;
      halfW = Math.max(size.x, height * 0.35) * 0.6;
    }
    const halfV = (top - bottom) / 2;
    const dist = Math.max(halfV / t, halfW / (t * aspect));
    // Pin the top of the frame to the crown: spare vertical room goes below (more chest), so the
    // face stays in the upper third whatever the aspect ratio.
    const visibleHalfV = dist * t;
    const cy = framing === 'bust' ? top - visibleHalfV : (top + bottom) / 2;
    const camY = framing === 'bust' ? eyePos.y : cy + height * 0.03;
    camGoal.target.set(headPos.x, cy, faceCenter.z);
    camGoal.pos.set(headPos.x, camY, faceCenter.z + Math.sqrt(Math.max(dist * dist - (camY - cy) ** 2, dist * dist * 0.5)));
    camGoal.fov = fov;
  }

  function applyCamera(dt: number, snap: boolean): void {
    if (snap || !camNow.init || reducedMotion) {
      camNow.pos.copy(camGoal.pos);
      camNow.target.copy(camGoal.target);
      camNow.fov = camGoal.fov;
      camNow.init = true;
    } else {
      camNow.pos.lerp(camGoal.pos, 1 - Math.exp(-dt / 0.25));
      camNow.target.lerp(camGoal.target, 1 - Math.exp(-dt / 0.25));
      camNow.fov = damp(camNow.fov, camGoal.fov, 0.25, dt);
    }
    camera.position.copy(camNow.pos);
    camera.fov = camNow.fov;
    camera.near = Math.max(0.01, camNow.pos.distanceTo(camNow.target) * 0.05);
    camera.far = camNow.pos.distanceTo(camNow.target) * 20 + height * 4;
    camera.lookAt(camNow.target);
    camera.updateProjectionMatrix();
  }

  // ---------- Animation state ----------
  const mql = prefersReducedMotion();
  let reducedOverride: boolean | null = options.reducedMotion ?? null;
  let reducedMotion = reducedOverride ?? mql?.matches ?? false;
  const animator = new Animator({ reducedMotion });
  if (options.environment) {
    const chest = rig.bonePosition('chest') ?? new Vector3(headPos.x, bbox.min.y + height * 0.7, headPos.z);
    env = new StageEnvironment(
      renderer,
      scene,
      camera,
      {
        height,
        floorY: bbox.min.y,
        center: new Vector3((bbox.min.x + bbox.max.x) / 2, bbox.min.y, (bbox.min.z + bbox.max.z) / 2),
        chest,
        radius: Math.max(size.x, size.z) / 2,
      },
      { hemi, key, rim, rim2 },
      typeof options.environment === 'object' ? options.environment : {},
      () => resize(),
    );
    env.setReducedMotion(reducedMotion);
    (shadow.material as MeshBasicMaterial).opacity = 0.7;
  }
  const machine = new StageStateMachine('idle');
  const onMql = (): void => {
    if (reducedOverride === null) {
      reducedMotion = mql?.matches ?? false;
      animator.reducedMotion = reducedMotion;
      env?.setReducedMotion(reducedMotion);
    }
  };
  mql?.addEventListener?.('change', onMql);
  if (player) {
    machine.onChange((next) => player?.setState(next));
  }

  // ---------- Audio ----------
  const muteKey = options.muteStorageKey === undefined ? MUTE_STORAGE_KEY : options.muteStorageKey;
  // A stored preference wins over the `muted` default.
  let muted = (muteKey ? readStoredMuted(muteKey) : null) ?? options.muted ?? false;
  let audio: { ctx: AudioContext; analyser: AnalyserNode; gain: GainNode; lipsync: AnalyserLipSync } | null = null;
  let current: { src: AudioBufferSourceNode; end: () => void } | null = null;
  let speakToken = 0;
  const silentLip = new LipSyncProcessor();
  let lip: LipSyncFrame = silentLip.silence(0);

  function ensureAudio(): NonNullable<typeof audio> {
    if (audio) return audio;
    const Ctor = audioContextCtor();
    if (!Ctor) throw new Error('WebAudio is not available');
    const ctx = new Ctor();
    const analyser = ctx.createAnalyser();
    const gain = ctx.createGain();
    gain.gain.value = muted ? 0 : 1;
    // The analyser sits before the mute gain, so a muted Lucina still moves her lips.
    analyser.connect(gain);
    gain.connect(ctx.destination);
    audio = { ctx, analyser, gain, lipsync: new AnalyserLipSync(analyser) };
    return audio;
  }

  async function unlockAudio(): Promise<void> {
    const a = ensureAudio();
    if (a.ctx.state !== 'running') {
      try {
        await a.ctx.resume();
      } catch {
        /* needs a gesture */
      }
    }
    // iOS: playing a silent buffer inside the gesture unlocks output.
    try {
      const b = a.ctx.createBuffer(1, 1, a.ctx.sampleRate);
      const s = a.ctx.createBufferSource();
      s.buffer = b;
      s.connect(a.ctx.destination);
      s.start();
    } catch {
      /* ignore */
    }
  }
  const gestureUnlock = (): void => {
    void unlockAudio().catch(() => {});
    if (audio?.ctx.state === 'running') removeGestureUnlock();
  };
  const removeGestureUnlock = (): void => {
    window.removeEventListener('pointerdown', gestureUnlock, true);
    window.removeEventListener('keydown', gestureUnlock, true);
    window.removeEventListener('touchend', gestureUnlock, true);
  };
  window.addEventListener('pointerdown', gestureUnlock, true);
  window.addEventListener('keydown', gestureUnlock, true);
  window.addEventListener('touchend', gestureUnlock, true);

  function stopSpeaking(): void {
    speakToken++;
    if (current) {
      const c = current;
      try {
        c.src.stop();
      } catch {
        /* not started */
      }
      c.end();
    }
  }

  async function speak(input: Blob | ArrayBuffer | ArrayBufferView, opts: SpeakOptions = {}): Promise<void> {
    if (disposed) return;
    stopSpeaking();
    const token = ++speakToken;
    const a = ensureAudio();
    if (a.ctx.state !== 'running') await a.ctx.resume().catch(() => {});
    let data: ArrayBuffer;
    if (input instanceof ArrayBuffer) data = input.slice(0);
    else if (ArrayBuffer.isView(input)) data = new Uint8Array(input.buffer, input.byteOffset, input.byteLength).slice().buffer;
    else data = await input.arrayBuffer();
    const buffer = await decode(a.ctx, data);
    if (token !== speakToken || disposed) return;

    const src = a.ctx.createBufferSource();
    src.buffer = buffer;
    src.connect(a.analyser);
    a.lipsync.processor.reset();
    machine.beginSpeaking();
    await new Promise<void>((resolve) => {
      let done = false;
      const end = (): void => {
        if (done) return;
        done = true;
        clearTimeout(guard);
        try {
          src.disconnect();
        } catch {
          /* ignore */
        }
        if (current?.src === src) current = null;
        if (!disposed) machine.endSpeaking();
        try {
          opts.onEnd?.();
        } finally {
          resolve();
        }
      };
      // If the context never starts (no user gesture on iOS), don't hang forever.
      const guard = setTimeout(end, (buffer.duration + 2) * 1000);
      src.onended = end;
      current = { src, end };
      src.start();
    });
  }

  function setMuted(m: boolean): void {
    muted = !!m;
    if (muteKey) writeMuted(muted, muteKey);
    if (audio) audio.gain.gain.setTargetAtTime(muted ? 0 : 1, audio.ctx.currentTime, 0.015);
  }

  // ---------- Pointer: look-at and taps ----------
  let lookEnabled = false;
  let pointerNdc: Vector2 | null = null;
  let pointerReleaseAt = 0;
  const raycaster = new Raycaster();
  const tapListeners = new Set<(e: TapEvent) => void>();
  let down: { id: number; x: number; y: number; t: number } | null = null;

  function toNdc(clientX: number, clientY: number, out = new Vector2()): Vector2 {
    const r = canvas.getBoundingClientRect();
    return out.set(((clientX - r.left) / Math.max(r.width, 1)) * 2 - 1, -((clientY - r.top) / Math.max(r.height, 1)) * 2 + 1);
  }
  const onPointerMove = (e: PointerEvent): void => {
    if (!lookEnabled) return;
    pointerNdc = toNdc(e.clientX, e.clientY, pointerNdc ?? new Vector2());
    pointerReleaseAt = e.pointerType === 'mouse' ? 0 : performance.now() + 1500;
  };
  const onPointerLeave = (e: PointerEvent): void => {
    if (e.pointerType === 'mouse' && !e.relatedTarget) pointerReleaseAt = performance.now() + 800;
    else if (e.pointerType !== 'mouse') pointerReleaseAt = performance.now() + 1200;
  };
  const onDown = (e: PointerEvent): void => {
    down = { id: e.pointerId, x: e.clientX, y: e.clientY, t: performance.now() };
    if (lookEnabled) onPointerMove(e);
  };
  const onUp = (e: PointerEvent): void => {
    const d = down;
    down = null;
    if (!d || d.id !== e.pointerId || !tapListeners.size) return;
    if (performance.now() - d.t > 700 || Math.hypot(e.clientX - d.x, e.clientY - d.y) > 12) return;
    raycaster.setFromCamera(toNdc(e.clientX, e.clientY), camera);
    const hit = raycaster.intersectObject(model, true).find((h) => h.object.visible && (h.object as Mesh).isMesh);
    if (!hit) return;
    const ev: TapEvent = {
      object: hit.object.name,
      point: { x: hit.point.x, y: hit.point.y, z: hit.point.z },
      clientX: e.clientX,
      clientY: e.clientY,
    };
    for (const cb of tapListeners) cb(ev);
  };
  window.addEventListener('pointermove', onPointerMove, { passive: true });
  window.addEventListener('pointerout', onPointerLeave, { passive: true });
  window.addEventListener('pointerup', onPointerLeave, { passive: true });
  canvas.addEventListener('pointerdown', onDown);
  canvas.addEventListener('pointerup', onUp);

  const _p = new Vector3();
  const _c = new Vector3();
  /** Converts the pointer (or the camera) into gaze angles relative to her eyes. */
  function updateLook(): void {
    container.updateMatrixWorld();
    const eyes = _p.copy(eyePos);
    // Direction to the camera, so "looking at the user" means looking into the lens.
    const toCam = _c.copy(camera.position).sub(eyes);
    const camYaw = Math.atan2(toCam.x, toCam.z);
    const camPitch = -Math.atan2(toCam.y, Math.hypot(toCam.x, toCam.z));
    if (pointerReleaseAt && performance.now() > pointerReleaseAt) {
      pointerNdc = null;
      pointerReleaseAt = 0;
    }
    if (!lookEnabled || !pointerNdc) {
      animator.setPointer({ yaw: camYaw, pitch: camPitch });
      return;
    }
    const faceNdc = eyes.clone().project(camera);
    const k = 0.55;
    animator.setPointer({
      yaw: camYaw + (pointerNdc.x - faceNdc.x) * k * Math.max(camera.aspect, 0.6),
      pitch: camPitch - (pointerNdc.y - faceNdc.y) * k,
    });
  }

  // ---------- Size / visibility / loop ----------
  function resize(): void {
    const w = Math.max(1, Math.round(canvas.clientWidth || canvas.width || 1));
    const h = Math.max(1, Math.round(canvas.clientHeight || canvas.height || 1));
    const dpr = Math.min(globalThis.devicePixelRatio || 1, maxDpr, env?.maxPixelRatio ?? Infinity);
    renderer.setPixelRatio(dpr);
    env?.setSize(w, h, dpr);
    renderer.setSize(w, h, false);
    camera.aspect = w / h;
    computeFraming();
    applyCamera(0, !camNow.init);
    if (!running()) renderOnce();
  }

  const stats = new FrameStats();
  let disposed = false;
  let raf = 0;
  let last = 0;
  let pageVisible = typeof document === 'undefined' ? true : !document.hidden;
  let onScreen = true;

  function running(): boolean {
    return !disposed && pageVisible && onScreen;
  }

  function step(dt: number): void {
    if (current && audio) lip = audio.lipsync.sample(dt);
    else lip = silentLip.silence(dt);
    updateLook();
    const f = motion.apply(animator.update(dt, machine.state, lip.weights, lip.stress), dt, machine.state, reducedMotion, !!player);
    if (player) {
      // clips underneath (frozen in reduced motion: a still idle pose, never the rest pose)
      rig.restoreBones();
      player.update(reducedMotion ? 0 : dt);
      rig.apply(f, { driven: player.drivenNodes });
    } else rig.apply(f);
    applyCamera(dt, false);
    if (env) {
      env.update(dt, machine.state, lip.open);
      env.driftCamera(camera, camNow.target);
      env.render(dt);
    } else renderer.render(scene, camera);
  }

  function renderOnce(): void {
    if (disposed) return;
    if (env) env.render(0);
    else renderer.render(scene, camera);
  }

  function frame(now: number): void {
    raf = 0;
    if (!running()) return;
    raf = requestAnimationFrame(frame);
    const dtMs = last ? now - last : 1000 / 60;
    last = now;
    const t0 = performance.now();
    step(Math.min(dtMs, 100) / 1000);
    stats.record(dtMs, performance.now() - t0, now);
  }

  function kick(): void {
    if (!raf && running()) {
      last = 0;
      raf = requestAnimationFrame(frame);
    }
  }

  const onVisibility = (): void => {
    pageVisible = !document.hidden;
    if (pageVisible) kick();
  };
  document.addEventListener('visibilitychange', onVisibility);
  let io: IntersectionObserver | null = null;
  if (typeof IntersectionObserver !== 'undefined') {
    io = new IntersectionObserver((entries) => {
      const e = entries[entries.length - 1];
      if (!e) return;
      onScreen = e.isIntersecting;
      if (onScreen) kick();
    });
    io.observe(canvas);
  }
  let ro: ResizeObserver | null = null;
  if ((options.autoResize ?? true) && typeof ResizeObserver !== 'undefined') {
    ro = new ResizeObserver(() => resize());
    ro.observe(canvas);
  }

  resize();
  step(0);
  kick();

  function dispose(): void {
    if (disposed) return;
    stopSpeaking();
    player?.dispose();
    disposed = true;
    if (raf) cancelAnimationFrame(raf);
    raf = 0;
    io?.disconnect();
    ro?.disconnect();
    mql?.removeEventListener?.('change', onMql);
    document.removeEventListener('visibilitychange', onVisibility);
    window.removeEventListener('pointermove', onPointerMove);
    window.removeEventListener('pointerout', onPointerLeave);
    window.removeEventListener('pointerup', onPointerLeave);
    canvas.removeEventListener('pointerdown', onDown);
    canvas.removeEventListener('pointerup', onUp);
    removeGestureUnlock();
    tapListeners.clear();
    stats.clear();
    env?.dispose();
    env = null;
    const textures = new Set<Texture>();
    const materials = new Set<Material>();
    scene.traverse((o) => {
      const m = o as Mesh;
      if (!m.isMesh) return;
      m.geometry?.dispose();
      for (const mat of Array.isArray(m.material) ? m.material : [m.material]) if (mat) materials.add(mat);
    });
    for (const mat of materials) {
      for (const v of Object.values(mat)) if (v && (v as Texture).isTexture) textures.add(v as Texture);
      mat.dispose();
    }
    for (const t of textures) t.dispose();
    envTex.dispose();
    renderer.dispose();
    if (audio) void audio.ctx.close().catch(() => {});
    audio = null;
  }

  const stage: LucinaStage = {
    get state() {
      return machine.state;
    },
    setState(s) {
      machine.set(s);
    },
    onStateChange(cb) {
      return machine.onChange(cb);
    },
    speak,
    stopSpeaking,
    get speaking() {
      return current !== null;
    },
    setMuted,
    isMuted: () => muted,
    setExpression(e, w = 1) {
      animator.setExpression(e, w);
    },
    onTap(cb) {
      tapListeners.add(cb);
      return () => tapListeners.delete(cb);
    },
    lookAtPointer(enabled) {
      lookEnabled = !!enabled;
      if (!lookEnabled) pointerNdc = null;
    },
    nod(strength = 1) {
      animator.nod(strength);
    },
    setFraming(f) {
      if (f !== 'bust' && f !== 'full') throw new TypeError(`Unknown framing: ${String(f)}`);
      framing = f;
      computeFraming();
    },
    setReducedMotion(r) {
      reducedOverride = r;
      reducedMotion = r ?? mql?.matches ?? false;
      animator.reducedMotion = reducedMotion;
      env?.setReducedMotion(reducedMotion);
    },
    gesture(name) {
      kick();
      return motion.play(name);
    },
    get gestures() {
      return clipNames();
    },
    react(at) {
      kick();
      animator.blink();
      env?.pulse(at);
      const reactions = player?.list().filter((c) => !c.loop && c.tags.some((t) => /reaction|happy|greeting/.test(t))) ?? [];
      if (player && reactions.length) {
        const pick = reactions[Math.floor(Math.random() * reactions.length)]!.name;
        void player.play(pick).catch(() => {});
        return pick;
      }
      return motion.react();
    },
    setQuality(q) {
      env?.setQuality(q);
    },
    get quality() {
      return env?.quality ?? null;
    },
    setTheme(theme) {
      env?.setTheme(theme);
    },
    unlockAudio,
    resize,
    play(name, opts) {
      if (!player) return Promise.reject(new Error('No clip file loaded (clipsUrl)'));
      kick();
      return player.play(name, opts);
    },
    stopClip(fade) {
      player?.stop(fade);
    },
    get clip() {
      return player?.current ?? null;
    },
    setStateClips(map) {
      player?.setStateClips(map);
    },
    dispose,
    stats,
    info: { placeholder, clips: player?.list() ?? [], extensions, rig: rig.report, hidden },
    three: { renderer, scene, camera, model },
  };
  return stage;
}
