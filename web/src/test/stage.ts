// Fake of src/lib/stageLoader for tests: no WebGL, no WebAudio, never any sound.
//   vi.mock("../lib/stageLoader", () => import("../test/stage"));

import type { ListenOptions, LucinaStageOptions, StageState, VoiceErrorCode } from "@secretary/lucina-stage";
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
  readonly setHairstyles = vi.fn();
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
} as unknown as StageModule;

export const loadStage = vi.fn(async (): Promise<StageModule> => module);
