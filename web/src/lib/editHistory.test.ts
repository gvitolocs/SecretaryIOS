import { describe, expect, it } from "vitest";
import { canRedo, canUndo, commit, MAX_HISTORY, redo, startHistory, undo } from "./editHistory";

const same = (a: number, b: number) => a === b;

describe("undo and redo", () => {
  it("steps back and forth, and a new change forgets the redo steps", () => {
    let h = startHistory(0);
    expect([canUndo(h), canRedo(h)]).toEqual([false, false]);
    h = commit(commit(commit(h, 1, same), 2, same), 3, same);
    expect(h.present).toBe(3);
    h = undo(undo(h));
    expect(h).toEqual({ past: [0], present: 1, future: [2, 3] });
    h = redo(h);
    expect(h.present).toBe(2);
    h = commit(h, 9, same);
    expect(h).toEqual({ past: [0, 1, 2], present: 9, future: [] });
    expect(canRedo(h)).toBe(false);
  });

  it("ignores a change that equals the present, and steps that aren't there", () => {
    const h = commit(startHistory(5), 5, same);
    expect(h).toEqual(startHistory(5));
    expect(undo(h)).toBe(h);
    expect(redo(h)).toBe(h);
  });

  it("keeps the last steps only", () => {
    let h = startHistory(0);
    for (let i = 1; i <= MAX_HISTORY + 20; i++) h = commit(h, i, same);
    expect(h.past).toHaveLength(MAX_HISTORY);
    expect(h.past[0]).toBe(20);
    expect(h.present).toBe(MAX_HISTORY + 20);
  });
});
