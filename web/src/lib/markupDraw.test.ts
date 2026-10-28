import { describe, expect, it } from "vitest";
import { MAX_MARKS, type Mark } from "./editList";
import { COLORS, extendMark, room, sizeOf, startMark, textMark, toFrame } from "./markupDraw";

describe("markup drawing rules", () => {
  it("maps a spot on the shown picture into the frame the marks are stored in", () => {
    expect(toFrame(0.5, 0.5, null)).toEqual([0.5, 0.5]);
    expect(toFrame(0, 0, { x: 0.25, y: 0.5, w: 0.5, h: 0.25 })).toEqual([0.25, 0.5]);
    expect(toFrame(1, 1, { x: 0.25, y: 0.5, w: 0.5, h: 0.25 })).toEqual([0.75, 0.75]);
    expect(toFrame(0.5, 0.5, { x: 0.25, y: 0.5, w: 0.5, h: 0.25 })).toEqual([0.5, 0.625]);
  });

  it("starts a stroke at the pointer and drops points that are too close", () => {
    let m: Mark = startMark("pen", COLORS[2]!, "medium", [0.1, 0.1]);
    expect(m).toEqual({ t: "pen", color: COLORS[2], width: sizeOf("pen", "medium"), points: [[0.1, 0.1]] });
    m = extendMark(m, [0.1005, 0.1], 0.002);
    expect((m as { points: unknown[] }).points).toHaveLength(1);
    m = extendMark(m, [0.2, 0.15], 0.002);
    expect((m as { points: unknown[] }).points).toHaveLength(2);
    const h = startMark("highlighter", "#ffcc00", "thick", [0, 0]);
    expect(h.t).toBe("highlighter");
    expect(sizeOf("highlighter", "medium")).toBeGreaterThan(sizeOf("pen", "thick"));
  });

  it("moves the tip of an arrow and leaves text alone", () => {
    const a = extendMark(startMark("arrow", "#fff", "thin", [0.2, 0.2]), [0.7, 0.4], 0.01);
    expect(a).toMatchObject({ t: "arrow", from: [0.2, 0.2], to: [0.7, 0.4] });
    const text = textMark("#fff", "medium", [0.1, 0.1], "hi")!;
    expect(extendMark(text, [0.5, 0.5], 0.01)).toBe(text);
  });

  it("makes text marks from what was typed", () => {
    expect(textMark("#fff", "thin", [0.1, 0.2], "  Ciao  ")).toEqual({ t: "text", color: "#fff", size: sizeOf("text", "thin"), x: 0.1, y: 0.2, text: "Ciao" });
    expect(textMark("#fff", "thin", [0, 0], "   ")).toBeNull();
    expect((textMark("#fff", "thin", [0, 0], "x".repeat(500)) as { text: string }).text).toHaveLength(300);
  });

  it("knows when the server's limits are reached", () => {
    expect(room([])).toEqual({ marks: true, points: true });
    const many: Mark[] = Array.from({ length: MAX_MARKS }, () => ({ t: "arrow", color: "#fff", width: 0.01, from: [0, 0], to: [1, 1] }));
    expect(room(many).marks).toBe(false);
    const dense: Mark = { t: "pen", color: "#fff", width: 0.01, points: Array.from({ length: 40_000 }, () => [0, 0] as [number, number]) };
    expect(room([dense]).points).toBe(false);
  });
});
