// Fake of src/lib/stageLoader for tests: no WebGL, no WebAudio, never any sound.
//   vi.mock("../lib/stageLoader", () => import("../test/stage"));

import type { ListenOptions, LucinaStageOptions, StageState, VoiceErrorCode } from "@secretary/lucina-stage";
import * as real from "@secretary/lucina-stage";
import { Group } from "three";
import { vi } from "vitest";
import type { StageModule } from "../lib/stageLoader";

export class FakeVoiceError extends Error {
  constructor(readonly code: VoiceErrorCode) {
    super(code);
  }
}

export class FakeStage {
  state: StageState = "idle";
  muted: boolean;
  states: StageState[] = [];
  private taps: ((e: { point: { x: number; y: number; z: number } }) => void)[] = [];
  private touches: ((e: { line: string; region: string }) => void)[] = [];
  speaking = false;
  readonly onTouch = vi.fn((cb: (e: { line: string; region: string }) => void) => {
    this.touches.push(cb);
    return () => {};
  });
  readonly cameraPreset = vi.fn();
  background = "default";
  lighting = "default";
  dancing = false;
  gyro = false;
  info = { clips: [{ name: "greet_wave", loop: false }, { name: "bow", loop: false }, { name: "status_loop", loop: true }] };
  gestures = ["nod", "tilt", "dance"];
  readonly play = vi.fn(async (_name: string, _opts?: unknown) => {});
  readonly setExpression = vi.fn();
  readonly setInteractive = vi.fn();
  readonly setBackground = vi.fn((id: string) => {
    this.background = id;
  });
  readonly setLighting = vi.fn((id: string) => {
    this.lighting = id;
  });
  readonly setDance = vi.fn(async (on: boolean, _opts?: unknown) => {
    this.dancing = on;
  });
  readonly setGyro = vi.fn();
  readonly snapshot = vi.fn(async () => new Blob(["png"], { type: "image/png" }));
  readonly resetView = vi.fn();
  readonly setView = vi.fn();
  private stateListeners: ((next: StageState, prev: StageState) => void)[] = [];
  readonly setState = vi.fn((s: StageState) => {
    const prev = this.state;
    this.state = s;
    this.states.push(s);
    if (prev !== s) for (const cb of this.stateListeners) cb(s, prev);
  });
  readonly onStateChange = vi.fn((cb: (next: StageState, prev: StageState) => void) => {
    this.stateListeners.push(cb);
    return () => {};
  });
  readonly react = vi.fn((_at?: unknown) => "nod");
  readonly gesture = vi.fn((_name: string) => true);
  readonly setTheme = vi.fn();
  readonly setQuality = vi.fn();
  readonly speak = vi.fn(async (_audio: unknown) => {});
  readonly stopSpeaking = vi.fn();
  readonly setMuted = vi.fn((m: boolean) => {
    this.muted = m;
  });
  readonly isMuted = () => this.muted;
  readonly onTap = vi.fn((cb: (e: { point: { x: number; y: number; z: number } }) => void) => {
    this.taps.push(cb);
    return () => {};
  });
  readonly nod = vi.fn();
  readonly setFraming = vi.fn();
  readonly unlockAudio = vi.fn(async () => {});
  readonly dispose = vi.fn();
  // Wardrobe (createWardrobeStage).
  outfit: string | null = null;
  wardrobe: unknown[] = [];
  private changingListeners: ((c: boolean) => void)[] = [];
  readonly setWardrobe = vi.fn((list: unknown[]) => {
    this.wardrobe = list;
  });
  readonly setOutfit = vi.fn(async (id: string) => {
    for (const cb of this.changingListeners) cb(true);
    this.outfit = id;
    for (const cb of this.changingListeners) cb(false);
  });
  hair = "natural";
  hairColor: string | null = null;
  hairstyles: unknown[] = [];
  readonly setHairstyles = vi.fn((list: unknown[]) => {
    this.hairstyles = list;
  });
  readonly onHairChange = vi.fn(() => () => {});
  readonly onOutfitChange = vi.fn(() => () => {});
  // The editor (src/editor/) works on a real, empty three.js model.
  readonly three = { model: new Group() };
  readonly refit = vi.fn();
  readonly setHair = vi.fn(async (id: string | null) => {
    this.hair = id ?? "natural";
  });
  readonly setHairColor = vi.fn(async (id: string | null) => {
    this.hairColor = id;
  });
  readonly onChanging = vi.fn((cb: (c: boolean) => void) => {
    this.changingListeners.push(cb);
    return () => {};
  });

  constructor(
    readonly canvas: HTMLCanvasElement,
    readonly options: LucinaStageOptions & { outfit?: string | null; hair?: string | null; hairColor?: string | null },
  ) {
    this.muted = options.muted ?? false;
    this.outfit = options.outfit ?? null;
    this.hair = options.hair ?? "natural";
    this.hairColor = options.hairColor ?? null;
  }

  /** Simulates a touch on her body (the stage already reacted; the host may speak `line`). */
  touch(region = "head", line = region): void {
    for (const cb of this.touches) cb({ region, line });
  }

  /** Simulates a tap on Lucina. */
  tap(): void {
    for (const cb of this.taps) cb({ point: { x: 0, y: 1.4, z: 0 } });
  }
}

export const fake = {
  stages: [] as FakeStage[],
  /** Make createLucinaStage reject (no WebGL). */
  failStage: false,
  listenSupported: true,
  listen: vi.fn(async (_opts?: ListenOptions): Promise<string> => "ciao Lucina"),
  speak: vi.fn(async (_text: string, _opts?: unknown): Promise<Blob> => new Blob(["fake-audio"])),
  voiceOptions: [] as unknown[],
};

export function resetFakeStage(): void {
  fake.stages = [];
  fake.failStage = false;
  fake.listenSupported = true;
  fake.voiceOptions = [];
  fake.listen.mockReset().mockImplementation(async () => "ciao Lucina");
  fake.speak.mockReset().mockImplementation(async () => new Blob(["fake-audio"]));
  loadStage.mockClear();
}

const createStage = async (canvas: HTMLCanvasElement, options: LucinaStageOptions = {}) => {
  if (fake.failStage) throw new Error("WebGL is not available");
  const s = new FakeStage(canvas, options);
  fake.stages.push(s);
  return s;
};

/** The panel re-dresses her through this when the worn look changes. */
export const fakeApplyLook = vi.fn(async (_model: unknown, _look: unknown, _ctx?: unknown) => {});

const module = {
  createLucinaStage: createStage,
  createWardrobeStage: createStage,
  createVoice: (opts: unknown) => {
    fake.voiceOptions.push(opts);
    return {
      speak: fake.speak,
      listen: fake.listen,
      get listenSupported() {
        return fake.listenSupported;
      },
    };
  },
  isVoiceError: (e: unknown, code?: VoiceErrorCode) => e instanceof FakeVoiceError && (!code || e.code === code),
  // The character editor: the real pure code (looks, sliders, catalogue parsing, the editing
  // session), on the fake stage. applyLook is spied so tests can see the panel re-dress her.
  defaultLook: real.defaultLook,
  normalizeLook: real.normalizeLook,
  parseAccessories: real.parseAccessories,
  parseParts: real.parseParts,
  resolveParts: real.resolveParts,
  createAvatarEditor: real.createAvatarEditor,
  editorPrepareModel: real.editorPrepareModel,
  applyLook: fakeApplyLook,
  BODY_SLIDERS: real.BODY_SLIDERS,
  FACE_SLIDERS: real.FACE_SLIDERS,
  MAKEUP_LAYERS: real.MAKEUP_LAYERS,
  ACCESSORY_SLOTS: real.ACCESSORY_SLOTS,
  PART_SLOTS: real.PART_SLOTS,
  SWATCHES: real.SWATCHES,
} as unknown as StageModule;

export const loadStage = vi.fn(async (): Promise<StageModule> => module);
