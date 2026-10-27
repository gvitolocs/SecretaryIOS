// The photo viewer core (crates/secretary-image-wasm, as image-core.wasm): zoom, pan, double
// tap and the swipe down to close, with the same rules the iPhone runs through
// secretary_image_view, so the gallery viewer behaves alike on the phone, the web and the Mac.
//
// One JSON request in, one JSON answer out (secretary_image::view::handle_json). The viewer
// still shows pictures (fitted) while the module loads or if it can't load; only zoom waits.

import wasmUrl from "./image-core.wasm?url";

interface Exports {
  memory: WebAssembly.Memory;
  alloc(len: number): number;
  dealloc(ptr: number, len: number): void;
  image_view(ptr: number, len: number): bigint;
}

/** The view state the core keeps; the host only passes it back. */
export interface CoreView {
  image_w: number;
  image_h: number;
  viewport_w: number;
  viewport_h: number;
  scale: number;
  offset_x: number;
  offset_y: number;
}

export interface Rect { x: number; y: number; w: number; h: number }

export type Gesture =
  | { op: "pan"; dx: number; dy: number }
  | { op: "pinch"; factor: number; x: number; y: number }
  | { op: "double_tap"; x: number; y: number }
  | { op: "resize"; image_w: number; image_h: number; viewport_w: number; viewport_h: number };

export type DragAxis = "undecided" | "pager" | "out" | "pan";

export interface DragOut { offset_x: number; offset_y: number; scale: number; backdrop: number }

export interface ViewRequest {
  view?: CoreView;
  new?: [number, number, number, number];
  gesture?: Gesture;
  drag?: { dx: number; dy: number; predicted_dy?: number; ended?: boolean };
  preview_px?: number;
  screen_scale?: number;
}

export interface ViewAnswer {
  ok: boolean;
  error?: string;
  view?: CoreView;
  rect?: Rect;
  wants_original?: boolean;
  drag?: { axis: DragAxis; out: DragOut; end?: "dismiss" | "info" | "back" };
}

export class ImageCore {
  private readonly exports: Exports;
  private readonly encoder = new TextEncoder();
  private readonly decoder = new TextDecoder();

  constructor(instance: WebAssembly.Instance) {
    this.exports = instance.exports as unknown as Exports;
  }

  view(request: ViewRequest): ViewAnswer {
    const bytes = this.encoder.encode(JSON.stringify(request));
    const ptr = this.exports.alloc(bytes.length);
    new Uint8Array(this.exports.memory.buffer, ptr, bytes.length).set(bytes);
    let packed: bigint;
    try {
      packed = this.exports.image_view(ptr, bytes.length);
    } finally {
      this.exports.dealloc(ptr, bytes.length);
    }
    const out = Number(packed >> 32n);
    const len = Number(packed & 0xffffffffn);
    const text = this.decoder.decode(new Uint8Array(this.exports.memory.buffer, out, len));
    this.exports.dealloc(out, len);
    return JSON.parse(text) as ViewAnswer;
  }
}

let core: Promise<ImageCore | null> | null = null;

/** The shared core, compiled once per page; null when the module can't load. */
export function imageCore(): Promise<ImageCore | null> {
  core ??= (async () => {
    try {
      const response = await fetch(wasmUrl);
      if (!response.ok) return null;
      const { instance } = await WebAssembly.instantiate(await response.arrayBuffer());
      return new ImageCore(instance);
    } catch {
      return null;
    }
  })();
  return core;
}

/** For tests: a core from raw module bytes. */
export async function imageCoreFromBytes(bytes: BufferSource): Promise<ImageCore> {
  const { instance } = await WebAssembly.instantiate(bytes);
  return new ImageCore(instance);
}

/** For tests: use this core (or none) instead of fetching the module. */
export function setImageCoreForTests(value: ImageCore | null): void {
  core = Promise.resolve(value);
}
