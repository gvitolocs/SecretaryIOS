import { createRoot } from "solid-js";
import { describe, expect, it } from "vitest";
import { emptyEdit, setAdjust, setFilter } from "../../../lib/editList";
import { createEditorModel } from "./editorModel";

function model(initial = emptyEdit()) {
  return createRoot((dispose) => ({ m: createEditorModel(initial), dispose }));
}

describe("the editor's state", () => {
  it("follows a slider while it moves and records one step when it is let go", () => {
    const { m, dispose } = model();
    expect([m.dirty(), m.canUndo(), m.identity()]).toEqual([false, false, true]);
    for (const v of [0.1, 0.4, 0.8]) m.preview(setAdjust(m.committed(), "exposure", v));
    expect(m.list().adjust.exposure).toBe(0.8);
    expect(m.committed().adjust.exposure).toBe(0);
    expect(m.dirty()).toBe(true);
    m.commit(setAdjust(m.committed(), "exposure", 0.8));
    expect(m.committed().adjust.exposure).toBe(0.8);
    // One drag, one undo step.
    m.undo();
    expect(m.list().adjust.exposure).toBe(0);
    expect(m.canUndo()).toBe(false);
    expect(m.canRedo()).toBe(true);
    m.redo();
    expect(m.list().adjust.exposure).toBe(0.8);
    expect(m.canRedo()).toBe(false);
    dispose();
  });

  it("undo drops a draft first, and a drag back to where it began leaves no step", () => {
    const { m, dispose } = model();
    m.commit(setAdjust(m.committed(), "contrast", 30));
    m.preview(setAdjust(m.committed(), "contrast", 60));
    expect(m.canUndo()).toBe(true);
    m.undo();
    expect(m.list().adjust.contrast).toBe(30);
    m.preview(setAdjust(m.committed(), "contrast", 50));
    m.commit(setAdjust(m.committed(), "contrast", 30));
    m.undo();
    expect(m.list().adjust.contrast).toBe(0);
    m.cancelPreview();
    dispose();
  });

  it("tells a changed list from the one the editor opened with, and the identity from both", () => {
    const stored = setFilter(emptyEdit(), "warm");
    const { m, dispose } = model(stored);
    expect([m.dirty(), m.identity()]).toEqual([false, false]);
    m.commit(setFilter(m.committed(), "cool"));
    expect(m.dirty()).toBe(true);
    m.undo();
    expect(m.dirty()).toBe(false);
    m.commit(setFilter(m.committed(), null));
    expect([m.dirty(), m.identity()]).toEqual([true, true]);
    expect(m.result().filter).toBeNull();
    dispose();
  });

  it("hands back a tidy list to save, and does not share state with what it was given", () => {
    const initial = emptyEdit();
    const { m, dispose } = model(initial);
    m.commit({ ...m.committed(), straighten: 1.23456, crop: { x: 0.1, y: 0.1, w: 0.5, h: 0.5 } });
    expect(m.result().straighten).toBe(1.23);
    expect(initial.crop).toBeNull();
    dispose();
  });
});
