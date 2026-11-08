// The player core (crates/secretary-player, as player-core.wasm): the queue, resume
// positions, subtitle parsing and the remembered subtitle choice. The iPhone runs the same
// engine through secretary-ffi, so "resume", "finished" and the subtitle pick agree.
//
// One `{"cmd": ...}` JSON request in, one JSON answer out (ABI: crates/secretary-player-wasm).
// The engine's state lives in localStorage and is saved after every request that changes it.

import wasmUrl from "./player-core.wasm?url";

const STORAGE_KEY = "secretary.player.v1";

/** Commands that never change the engine's state (no save needed). */
const READ_ONLY = new Set(["queue_get", "resume", "pending", "recent", "subtitles", "plan", "subtitle_pick"]);

interface Exports {
  memory: WebAssembly.Memory;
  alloc(len: number): number;
  dealloc(ptr: number, len: number): void;
  player_init(ptr: number, len: number): void;
  player_request(ptr: number, len: number, nowMs: number): bigint;
  player_state(): bigint;
}

export type CoreAnswer = { ok: boolean; code?: string; error?: string } & Record<string, unknown>;

export class PlayerCore {
  private readonly exports: Exports;
  private readonly encoder = new TextEncoder();
  private readonly decoder = new TextDecoder();

  constructor(instance: WebAssembly.Instance, savedState: string) {
    this.exports = instance.exports as unknown as Exports;
    this.withBuffer(savedState, (ptr, len) => this.exports.player_init(ptr, len));
  }

  private withBuffer<T>(text: string, use: (ptr: number, len: number) => T): T {
    const bytes = this.encoder.encode(text);
    const ptr = bytes.length ? this.exports.alloc(bytes.length) : 0;
    if (bytes.length) new Uint8Array(this.exports.memory.buffer, ptr, bytes.length).set(bytes);
    try {
      return use(ptr, bytes.length);
    } finally {
      if (bytes.length) this.exports.dealloc(ptr, bytes.length);
    }
  }

  /** Reads and frees an answer packed as `ptr << 32 | len`. */
  private take(packed: bigint): string {
    const ptr = Number(packed >> 32n);
    const len = Number(packed & 0xffffffffn);
    const text = this.decoder.decode(new Uint8Array(this.exports.memory.buffer, ptr, len));
    this.exports.dealloc(ptr, len);
    return text;
  }

  request(command: { cmd: string } & Record<string, unknown>, nowMs = Date.now()): CoreAnswer {
    const answer = this.withBuffer(JSON.stringify(command), (ptr, len) => this.take(this.exports.player_request(ptr, len, nowMs)));
    const parsed = JSON.parse(answer) as CoreAnswer;
    if (!READ_ONLY.has(command.cmd)) save(this.state());
    return parsed;
  }

  state(): string {
    return this.take(this.exports.player_state());
  }
}

function load(): string {
  try {
    return localStorage.getItem(STORAGE_KEY) ?? "";
  } catch {
    return "";
  }
}

function save(state: string): void {
  try {
    localStorage.setItem(STORAGE_KEY, state);
  } catch {
    // Private mode or full storage: positions still reach the server.
  }
}

let core: Promise<PlayerCore> | null = null;

/** The shared core, compiled once per page. */
export function playerCore(): Promise<PlayerCore> {
  core ??= (async () => {
    const response = await fetch(wasmUrl);
    const bytes = await response.arrayBuffer();
    const { instance } = await WebAssembly.instantiate(bytes);
    return new PlayerCore(instance, load());
  })();
  return core;
}

/** For tests: a core from raw module bytes. */
export async function coreFromBytes(bytes: BufferSource, savedState = ""): Promise<PlayerCore> {
  const { instance } = await WebAssembly.instantiate(bytes);
  return new PlayerCore(instance, savedState);
}
