import { describe, expect, it } from "vitest";
import {
  clipboardMessage,
  CLIPBOARD_MAX_BYTES,
  createWheel,
  encodeVideo,
  fitRect,
  isMacPaste,
  keyButton,
  keyMessage,
  mouse,
  mouseMask,
  parseServer,
  parseVideo,
  textMessages,
  toHost,
} from "./deskWire";

const key = (k: string, mods: Partial<{ ctrlKey: boolean; altKey: boolean; shiftKey: boolean; metaKey: boolean; isComposing: boolean }> = {}) => ({
  key: k,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  metaKey: false,
  ...mods,
});

describe("deskWire video frames", () => {
  it("parses the 12-byte header (CONTRACT §4)", () => {
    const data = new Uint8Array([9, 8, 7]);
    const buf = encodeVideo({ codec: "vp9", key: true, last: true, display: 0, pts: 123456789012, data });
    const f = parseVideo(buf)!;
    expect(f.codec).toBe("vp9");
    expect(f.key).toBe(true);
    expect(f.last).toBe(true);
    expect(f.display).toBe(0);
    expect(f.pts).toBe(123456789012);
    expect([...f.data]).toEqual([9, 8, 7]);
  });

  it("reads the flags and codec bytes as laid out", () => {
    const raw = new Uint8Array(13);
    raw.set([1, 5, 2, 0]);
    raw[4] = 7; // pts = 7, little-endian
    raw[12] = 42;
    const f = parseVideo(raw.buffer)!;
    expect(f).toMatchObject({ codec: "av1", key: false, last: true, pts: 7 });
    expect([...f.data]).toEqual([42]);
  });

  it("ignores other kinds, unknown codecs and short messages", () => {
    const raw = new Uint8Array(12);
    raw.set([2, 1]);
    expect(parseVideo(raw.buffer)).toBeNull();
    raw.set([1, 9]);
    expect(parseVideo(raw.buffer)).toBeNull();
    expect(parseVideo(new ArrayBuffer(5))).toBeNull();
  });
});

describe("deskWire server messages", () => {
  it("accepts known types and ignores unknown ones", () => {
    expect(parseServer('{"type":"hello","v":1,"mode":"watch","sandbox":"workspace"}')).toMatchObject({ type: "hello", mode: "watch" });
    expect(parseServer('{"type":"future","x":1}')).toBeNull();
    expect(parseServer("not json")).toBeNull();
    expect(parseServer('{"type":"login","displays":[]}')).toBeNull();
    expect(parseServer('{"type":"login","hostname":"h","platform":"Linux","version":"1.4.9","currentDisplay":0,"displays":[{"x":0,"y":0,"width":1280,"height":800}]}')).toMatchObject({
      type: "login",
    });
  });
});

describe("deskWire input", () => {
  it("builds mouse masks as (buttons << 3) | kind", () => {
    expect(mouseMask("move")).toBe(0);
    expect(mouseMask("down", "left")).toBe(9);
    expect(mouseMask("up", "right")).toBe(18);
    expect(mouseMask("down", "middle")).toBe(33);
    expect(mouseMask("wheel")).toBe(3);
    expect(mouse("down", 10.6, 3.2, "left")).toEqual({ type: "mouse", mask: 9, x: 11, y: 3 });
  });

  it("splits long text at 1024 characters, not in the middle of a character", () => {
    const long = "😀".repeat(1500);
    const parts = textMessages(long);
    expect(parts).toHaveLength(2);
    expect(Array.from((parts[0] as { text: string }).text)).toHaveLength(1024);
    expect(Array.from((parts[1] as { text: string }).text)).toHaveLength(476);
    expect(textMessages("")).toEqual([]);
  });

  it("maps keys: text for plain typing, ControlKey names, ⌘ as Control", () => {
    expect(keyMessage(key("a"))).toEqual({ type: "text", text: "a" });
    expect(keyMessage(key("A", { shiftKey: true }))).toEqual({ type: "text", text: "A" });
    expect(keyMessage(key(" "))).toEqual({ type: "text", text: " " });
    expect(keyMessage(key("Enter"))).toEqual({ type: "key", key: "Return" });
    expect(keyMessage(key("ArrowLeft", { shiftKey: true }))).toEqual({ type: "key", key: "LeftArrow", modifiers: ["Shift"] });
    expect(keyMessage(key("l", { metaKey: true }))).toEqual({ type: "key", char: "l", modifiers: ["Control"] });
    expect(keyMessage(key("T", { ctrlKey: true, shiftKey: true }))).toEqual({ type: "key", char: "t", modifiers: ["Control", "Shift"] });
    expect(keyMessage(key("F5"))).toEqual({ type: "key", key: "F5" });
    expect(keyMessage(key("Shift", { shiftKey: true }))).toBeNull();
    expect(keyMessage(key("a", { isComposing: true }))).toBeNull();
    expect(keyMessage(key("Dead"))).toBeNull();
  });

  it("key buttons send ControlKey names or chars", () => {
    expect(keyButton("Escape")).toEqual({ type: "key", key: "Escape" });
    expect(keyButton("l", ["Control"])).toEqual({ type: "key", char: "l", modifiers: ["Control"] });
  });
});

describe("deskWire geometry", () => {
  it("letterboxes and maps to absolute host pixels", () => {
    const box = { left: 0, top: 0, width: 390, height: 400 };
    const shown = fitRect(box, { width: 1280, height: 800 });
    expect(shown.width).toBe(390);
    expect(shown.height).toBeCloseTo(243.75);
    expect(shown.top).toBeCloseTo(78.125);
    const d = { x: 100, y: 50, width: 1280, height: 800 };
    expect(toHost({ clientX: 195, clientY: 200 }, shown, d)).toEqual({ x: 740, y: 450 });
    // Outside the picture: clamped to the display's edge.
    expect(toHost({ clientX: -20, clientY: 1000 }, shown, d)).toEqual({ x: 100, y: 849 });
  });

  it("accumulates trackpad wheel deltas into steps, y > 0 up", () => {
    const w = createWheel(50);
    expect(w(0, 20)).toBeNull();
    expect(w(0, 40)).toEqual({ x: 0, y: -1 });
    expect(w(0, -120)).toEqual({ x: 0, y: 2 });
    // A mouse notch in lines (Firefox: 3 lines) is one step.
    expect(createWheel(50)(0, 3, 1)).toEqual({ x: 0, y: -1 });
  });
});

describe("deskWire clipboard", () => {
  it("parses the clipboard frame and needs its text", () => {
    expect(parseServer('{"type":"clipboard","text":"hej"}')).toEqual({ type: "clipboard", text: "hej" });
    expect(parseServer('{"type":"clipboard"}')).toBeNull();
  });

  it("caps a paste at 60,000 UTF-8 bytes, not characters", () => {
    expect(clipboardMessage("a".repeat(CLIPBOARD_MAX_BYTES))).toEqual({ type: "clipboard", text: "a".repeat(CLIPBOARD_MAX_BYTES) });
    expect(clipboardMessage("a".repeat(CLIPBOARD_MAX_BYTES + 1))).toBeNull();
    // 3 bytes each in UTF-8: 20,001 of them are over the limit though far under 60,000 characters.
    expect(clipboardMessage("€".repeat(20_000))).not.toBeNull();
    expect(clipboardMessage("€".repeat(20_001))).toBeNull();
  });

  it("maps ⌘C/⌘X to Control, keeps ⌘V for the page and the Mac's own shortcuts local", () => {
    expect(keyMessage(key("c", { metaKey: true }))).toEqual({ type: "key", char: "c", modifiers: ["Control"] });
    expect(keyMessage(key("x", { metaKey: true }))).toEqual({ type: "key", char: "x", modifiers: ["Control"] });
    expect(isMacPaste(key("v", { metaKey: true }))).toBe(true);
    expect(isMacPaste(key("v", { ctrlKey: true }))).toBe(false);
    for (const k of ["q", "w", "h", "m", "Tab", "`", " "]) expect(keyMessage(key(k, { metaKey: true }))).toBeNull();
    // Ctrl is Lucina's own: Ctrl+W closes her tab, it is not the Mac's.
    expect(keyMessage(key("w", { ctrlKey: true }))).toEqual({ type: "key", char: "w", modifiers: ["Control"] });
  });
});
