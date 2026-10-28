// The photo viewer core (crates/secretary-image-wasm, as image-core.wasm): zoom, pan, double
// tap and the swipe down to close, with the same rules the iPhone runs through
// secretary_image_view, so the gallery viewer behaves alike on the phone, the web and the Mac.
//
// One JSON request in, one JSON answer out (secretary_image::view::handle_json). The viewer
// still shows pictures (fitted) while the module loads or if it can't load; only zoom waits.
//
// The same module renders the photo editor's edit lists (crates/secretary-image/src/edit.rs): `editSource` puts a
// picture's RGBA pixels into the module's memory once, and every `render` of an edit list reads them from there and
// hands back the edited pixels, so a slider can drive a live preview.

import type { Adjust, EditList } from "./editList";

import wasmUrl from "./image-core.wasm?url";

interface Exports {
  memory: WebAssembly.Memory;
  alloc(len: number): number;
  dealloc(ptr: number, len: number): void;
  image_view(ptr: number, len: number): bigint;
  image_edit(src: number, width: number, height: number, json: number, jsonLen: number): bigint;
  image_auto(src: number, width: number, height: number): bigint;
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
  drag?: { dx: number; dy: number; vx?: number; vy?: number; predicted_dy?: number; ended?: boolean };
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

/** The core refused an edit list or a picture (the message is the core's). */
export class EditError extends Error {}

/** Edited pixels: 8-bit RGBA, rows top to bottom. */
export interface Pixels {
  width: number;
  height: number;
  rgba: Uint8ClampedArray<ArrayBuffer>;
}

export class ImageCore {
  private readonly exports: Exports;
  private readonly encoder = new TextEncoder();
  private readonly decoder = new TextDecoder();

  constructor(instance: WebAssembly.Instance) {
    this.exports = instance.exports as unknown as Exports;
  }

  /** Copy a picture into the module once; render edit lists of it with the returned source, then `free` it. */
  editSource(rgba: Uint8ClampedArray | Uint8Array, width: number, height: number): EditSource {
    if (rgba.length !== width * height * 4) throw new EditError("the pixel buffer does not match the size");
    const ptr = this.exports.alloc(rgba.length);
    new Uint8Array(this.exports.memory.buffer, ptr, rgba.length).set(rgba);
    return new EditSource(this.exports, this.encoder, this.decoder, ptr, width, height);
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

/** A picture living in the core's memory, to render edit lists of. */
export class EditSource {
  private ptr: number;

  constructor(
    private readonly exports: Exports,
    private readonly encoder: TextEncoder,
    private readonly decoder: TextDecoder,
    ptr: number,
    readonly width: number,
    readonly height: number,
  ) {
    this.ptr = ptr;
  }

  private take(packed: bigint): { out: number; len: number } {
    return { out: Number(packed >> 32n), len: Number(packed & 0xffffffffn) };
  }

  /** The picture under `list` (an object, or its JSON). Throws `EditError` for a list the core refuses. */
  render(list: EditList | string): Pixels {
    if (this.ptr === 0) throw new EditError("the source was freed");
    const json = this.encoder.encode(typeof list === "string" ? list : JSON.stringify(list));
    const jp = this.exports.alloc(json.length);
    new Uint8Array(this.exports.memory.buffer, jp, json.length).set(json);
    let packed: bigint;
    try {
      packed = this.exports.image_edit(this.ptr, this.width, this.height, jp, json.length);
    } finally {
      this.exports.dealloc(jp, json.length);
    }
    const { out, len } = this.take(packed);
    // [status, width, height] (u32, little endian), then the pixels or an error message.
    const head = new DataView(this.exports.memory.buffer, out, 12);
    const status = head.getUint32(0, true);
    const payload = new Uint8Array(this.exports.memory.buffer, out + 12, len - 12);
    try {
      if (status !== 0) throw new EditError(this.decoder.decode(payload));
      const rgba = new Uint8ClampedArray(payload.length);
      rgba.set(payload);
      return { width: head.getUint32(4, true), height: head.getUint32(8, true), rgba };
    } finally {
      this.exports.dealloc(out, len);
    }
  }

  /** Slider values that tune the picture's exposure, contrast and colour, from its histogram. */
  auto(): Adjust {
    if (this.ptr === 0) throw new EditError("the source was freed");
    const { out, len } = this.take(this.exports.image_auto(this.ptr, this.width, this.height));
    const text = this.decoder.decode(new Uint8Array(this.exports.memory.buffer, out, len));
    this.exports.dealloc(out, len);
    const answer = JSON.parse(text) as { ok: boolean; error?: string; adjust?: Adjust };
    if (!answer.ok || !answer.adjust) throw new EditError(answer.error ?? "auto enhance failed");
    return answer.adjust;
  }

  free(): void {
    if (this.ptr !== 0) this.exports.dealloc(this.ptr, this.width * this.height * 4);
    this.ptr = 0;
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
