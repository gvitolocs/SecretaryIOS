import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { addPendingNote, PENDING_NOTE_TTL_MS, pendingNotes, removePendingNote, stillPending, updatePendingNote } from "./voiceNotes";

beforeEach(() => vi.useFakeTimers());
afterEach(() => {
  for (const p of pendingNotes()) removePendingNote(p.key);
  vi.useRealTimers();
});

describe("pending voice notes", () => {
  it("shows the transcript early and goes when the timeline item arrives", () => {
    const key = addPendingNote();
    expect(stillPending(pendingNotes(), () => false)).toHaveLength(1);
    updatePendingNote(key, { transcript: "ciao" });
    expect(pendingNotes()[0]?.transcript).toBe("ciao");
    updatePendingNote(key, { userItemId: "u1" });
    expect(stillPending(pendingNotes(), (id) => id === "u1")).toEqual([]);
    expect(stillPending(pendingNotes(), () => false)).toHaveLength(1);
  });

  it("is dropped if the timeline never brings the item", () => {
    const key = addPendingNote();
    updatePendingNote(key, { userItemId: "u1" });
    vi.advanceTimersByTime(PENDING_NOTE_TTL_MS);
    expect(pendingNotes()).toEqual([]);
  });

  it("can be removed by hand (a failed note)", () => {
    const key = addPendingNote();
    removePendingNote(key);
    expect(pendingNotes()).toEqual([]);
  });
});
