/**
 * Body animation clips (lucina-anims.glb + clips.json) on an AnimationMixer.
 *
 * - The clip file has the model's skeleton (same node names) and no mesh; its tracks bind by node
 *   name to the loaded model, so one clip file serves every model built to the same bone contract.
 * - An idle clip is always playing underneath, so the rest pose never shows. One-shots crossfade
 *   in and back to the idle pool when they end; loops play until something else is asked for.
 * - Clips carry no face/eye tracks: blink, gaze and lip-sync stay procedural (see Rig.apply).
 */
import { AnimationAction, AnimationClip, AnimationMixer, LoopOnce, LoopRepeat, type Object3D } from 'three';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';
import { MeshoptDecoder } from 'three/addons/libs/meshopt_decoder.module.js';
import { fetchOk, type LoadOptions } from './loader';
import type { StageState } from './types';

export interface ClipInfo {
  name: string;
  duration: number;
  loop: boolean;
  tags: string[];
  /** Suggested expression morph to blend in while it plays (e.g. 'smile'). */
  expression?: string | null;
  description?: string;
}

export interface ClipLibrary {
  clips: AnimationClip[];
  info: Map<string, ClipInfo>;
}

export interface PlayOptions {
  /** Crossfade seconds (default 0.35). */
  fade?: number;
  /** Override the clip's loop flag. */
  loop?: boolean;
  /** Playback speed (default 1). */
  speed?: number;
  /** For one-shots: clip (or idle pool when omitted) to return to afterwards. */
  then?: string;
}

/** Default clips per conversation state, used when the library has them. */
export const DEFAULT_STATE_CLIPS: Record<StageState, string[]> = {
  idle: ['idle_breathe', 'idle_shift'],
  listening: ['listen_lean'],
  thinking: ['think_chin'],
  speaking: ['talk_gesture_1', 'talk_gesture_2', 'talk_gesture_3'],
};

/** Loads the clip GLB (auth via fetchInit) and its optional clips.json metadata. */
export async function loadClipLibrary(url: string, metaUrl: string | undefined, opts: LoadOptions = {}): Promise<ClipLibrary> {
  const [buf, meta] = await Promise.all([
    fetchOk(url, opts.fetchInit, opts.signal).then((r) => r.arrayBuffer()),
    metaUrl
      ? fetchOk(metaUrl, opts.fetchInit, opts.signal)
          .then((r) => r.json() as Promise<{ clips?: ClipInfo[] }>)
          .catch(() => ({ clips: [] as ClipInfo[] }))
      : Promise.resolve({ clips: [] as ClipInfo[] }),
  ]);
  const loader = new GLTFLoader();
  loader.setMeshoptDecoder(MeshoptDecoder);
  const gltf = await loader.parseAsync(buf, '');
  return clipLibraryFrom(gltf.animations, meta.clips ?? []);
}

export function clipLibraryFrom(clips: AnimationClip[], meta: ClipInfo[] = []): ClipLibrary {
  const info = new Map<string, ClipInfo>();
  const byName = new Map(meta.map((m) => [m.name, m]));
  for (const c of clips) {
    const m = byName.get(c.name);
    info.set(c.name, {
      name: c.name,
      duration: c.duration,
      loop: m?.loop ?? /idle|loop|lean|talk|think/.test(c.name),
      tags: m?.tags ?? [],
      expression: m?.expression ?? null,
      description: m?.description ?? '',
    });
  }
  return { clips, info };
}

interface Pending {
  resolve: () => void;
}

export class ClipPlayer {
  readonly mixer: AnimationMixer;
  private readonly actions = new Map<string, AnimationAction>();
  private readonly info: Map<string, ClipInfo>;
  private idlePool: string[] = [];
  private stateClips: Partial<Record<StageState, string[]>> = {};
  private active: AnimationAction | null = null;
  private activeName: string | null = null;
  private oneShot: { name: string; then?: string; pending: Pending[] } | null = null;
  private state: StageState = 'idle';
  private loopTime = 0;
  private readonly random: () => number;
  /** Node names animated by at least one clip. */
  readonly drivenNodes = new Set<string>();

  constructor(
    readonly root: Object3D,
    lib: ClipLibrary,
    opts: { random?: () => number; stateClips?: Partial<Record<StageState, string[]>> } = {},
  ) {
    this.mixer = new AnimationMixer(root);
    this.info = lib.info;
    this.random = opts.random ?? Math.random;
    const names = new Set<string>();
    root.traverse((o) => {
      if (o.name) names.add(o.name);
    });
    for (const clip of lib.clips) {
      // Keep only tracks whose node exists in this model (a missing optional bone must not throw).
      const tracks = clip.tracks.filter((t) => names.has(t.name.slice(0, t.name.lastIndexOf('.'))));
      if (!tracks.length) continue;
      for (const t of tracks) this.drivenNodes.add(t.name.slice(0, t.name.lastIndexOf('.')));
      const c = new AnimationClip(clip.name, clip.duration, tracks);
      const a = this.mixer.clipAction(c);
      this.actions.set(clip.name, a);
    }
    this.mixer.addEventListener('finished', (e) => this.onFinished(e.action as AnimationAction));
    const want = { ...DEFAULT_STATE_CLIPS, ...opts.stateClips };
    for (const [s, list] of Object.entries(want) as [StageState, string[]][]) {
      const ok = list.filter((n) => this.actions.has(n));
      if (ok.length) this.stateClips[s] = ok;
    }
    this.idlePool = this.stateClips.idle ?? [...this.actions.keys()].filter((n) => this.info.get(n)?.tags.includes('idle'));
    if (!this.idlePool.length && this.actions.size) this.idlePool = [this.actions.keys().next().value as string];
    this.startLoop(this.pick(this.idlePool), 0);
  }

  /** Clip names in the library (those that bound to this model). */
  list(): ClipInfo[] {
    return [...this.actions.keys()].map((n) => this.info.get(n)!);
  }

  has(name: string): boolean {
    return this.actions.has(name);
  }

  get current(): string | null {
    return this.activeName;
  }

  get ready(): boolean {
    return this.active !== null;
  }

  /** Replace the idle pool (clips that play when nothing else does). */
  setIdle(names: string[]): void {
    const ok = names.filter((n) => this.actions.has(n));
    if (ok.length) this.idlePool = ok;
  }

  /** Map conversation states to clip pools; `setState` then switches loops automatically. */
  setStateClips(map: Partial<Record<StageState, string[]>>): void {
    for (const [s, list] of Object.entries(map) as [StageState, string[]][]) {
      const ok = list.filter((n) => this.actions.has(n));
      if (ok.length) this.stateClips[s] = ok;
      else delete this.stateClips[s];
    }
    if (this.stateClips.idle) this.idlePool = this.stateClips.idle;
  }

  /** Follows the stage state: loops the state's clip pool unless a one-shot is playing. */
  setState(state: StageState): void {
    if (state === this.state) return;
    this.state = state;
    if (this.oneShot) return; // the one-shot returns to the state's pool when it ends
    this.startLoop(this.pick(this.poolFor(state)), 0.45);
  }

  /**
   * Plays a clip. Loops keep playing until the next play()/stop()/state change; the promise
   * resolves when a one-shot ends (or immediately for a loop). Unknown names reject.
   */
  play(name: string, opts: PlayOptions = {}): Promise<void> {
    const a = this.actions.get(name);
    if (!a) return Promise.reject(new Error(`Unknown clip: ${name}`));
    const loop = opts.loop ?? this.info.get(name)?.loop ?? false;
    const fade = opts.fade ?? 0.35;
    if (loop) {
      this.finishOneShot();
      this.startLoop(name, fade, opts.speed);
      return Promise.resolve();
    }
    this.finishOneShot();
    a.reset();
    a.setLoop(LoopOnce, 1);
    a.clampWhenFinished = true;
    a.setEffectiveTimeScale(opts.speed ?? 1);
    a.setEffectiveWeight(1);
    this.crossTo(a, fade);
    this.activeName = name;
    return new Promise((resolve) => {
      this.oneShot = { name, then: opts.then, pending: [{ resolve }] };
    });
  }

  /** Ends whatever plays and returns to the current state's pool. */
  stop(fade = 0.4): void {
    this.finishOneShot();
    this.startLoop(this.pick(this.poolFor(this.state)), fade);
  }

  update(dt: number): void {
    this.mixer.update(dt);
    // Rotate idle variations now and then (only between full loop cycles).
    if (!this.oneShot && this.active && this.activeName && this.poolFor(this.state).length > 1) {
      this.loopTime += dt;
      const d = this.active.getClip().duration;
      if (this.loopTime > Math.max(8, d * 2) && this.random() < dt * 0.15) {
        const next = this.pick(this.poolFor(this.state).filter((n) => n !== this.activeName));
        this.startLoop(next, 0.8);
      }
    }
  }

  dispose(): void {
    this.finishOneShot();
    this.mixer.stopAllAction();
    this.mixer.uncacheRoot(this.root);
  }

  // ---------------------------------------------------------------- internals

  private poolFor(state: StageState): string[] {
    return this.stateClips[state] ?? this.idlePool;
  }

  private pick(pool: string[]): string {
    return pool[Math.floor(this.random() * pool.length) % pool.length] ?? pool[0]!;
  }

  private startLoop(name: string | undefined, fade: number, speed = 1): void {
    if (!name) return;
    const a = this.actions.get(name);
    if (!a) return;
    if (a === this.active && a.isRunning()) return;
    a.reset();
    a.setLoop(LoopRepeat, Infinity);
    a.clampWhenFinished = false;
    a.setEffectiveTimeScale(speed);
    a.setEffectiveWeight(1);
    // Start loops at a random phase so idle variations do not restart visibly.
    a.time = this.random() * a.getClip().duration;
    this.crossTo(a, fade);
    this.activeName = name;
    this.loopTime = 0;
  }

  private crossTo(a: AnimationAction, fade: number): void {
    const prev = this.active;
    a.enabled = true;
    a.play();
    if (prev && prev !== a && fade > 0) {
      prev.crossFadeTo(a, fade, false);
    } else if (prev && prev !== a) {
      prev.stop();
    }
    this.active = a;
  }

  private onFinished(a: AnimationAction): void {
    if (!this.oneShot || this.actions.get(this.oneShot.name) !== a) return;
    const { then, pending } = this.oneShot;
    this.oneShot = null;
    const next = then && this.actions.has(then) ? then : this.pick(this.poolFor(this.state));
    const nextLoop = this.info.get(next)?.loop ?? true;
    if (nextLoop) this.startLoop(next, 0.5);
    else void this.play(next, { fade: 0.5 });
    for (const p of pending) p.resolve();
  }

  private finishOneShot(): void {
    if (!this.oneShot) return;
    const { pending } = this.oneShot;
    this.oneShot = null;
    for (const p of pending) p.resolve();
  }
}
