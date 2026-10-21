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
  private taps: (() => void)[] = [];
  readonly setState = vi.fn((s: StageState) => {
    this.state = s;
    this.states.push(s);
  });
  readonly speak = vi.fn(async (_audio: unknown) => {});
  readonly stopSpeaking = vi.fn();
  readonly setMuted = vi.fn((m: boolean) => {
    this.muted = m;
  });
  readonly isMuted = () => this.muted;
  readonly onTap = vi.fn((cb: () => void) => {
    this.taps.push(cb);
    return () => {};
  });
  readonly nod = vi.fn();
  readonly setFraming = vi.fn();
  readonly unlockAudio = vi.fn(async () => {});
  readonly dispose = vi.fn();

  constructor(readonly canvas: HTMLCanvasElement, readonly options: LucinaStageOptions) {
    this.muted = options.muted ?? false;
  }

  /** Simulates a tap on Lucina. */
  tap(): void {
    for (const cb of this.taps) cb();
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

const module = {
  createLucinaStage: async (canvas: HTMLCanvasElement, options: LucinaStageOptions = {}) => {
    if (fake.failStage) throw new Error("WebGL is not available");
    const s = new FakeStage(canvas, options);
    fake.stages.push(s);
    return s;
  },
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
