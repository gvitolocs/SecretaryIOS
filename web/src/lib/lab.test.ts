import { describe, expect, it } from "vitest";
import { labJob } from "../test/labFixtures";
import { isActive, LAB_SUGGESTIONS, pollDelay, POLL_ACTIVE_MS, POLL_IDLE_MS, progressLine, promptValid, readyCount, sortJobs, splitFromPointer, statusKey, withSuggestion } from "./lab";

describe("promptValid", () => {
  it("wants 1 to 1000 characters once trimmed, counting characters not UTF-16 units", () => {
    expect(promptValid("")).toBe(false);
    expect(promptValid("   \n ")).toBe(false);
    expect(promptValid(" a ")).toBe(true);
    expect(promptValid("x".repeat(1000))).toBe(true);
    expect(promptValid("x".repeat(1001))).toBe(false);
    expect(promptValid("😀".repeat(1000))).toBe(true);
  });
});

describe("withSuggestion", () => {
  it("fills an empty field and appends to text", () => {
    expect(withSuggestion("", "Make it warmer")).toBe("Make it warmer");
    expect(withSuggestion("  ", "Make it warmer")).toBe("Make it warmer");
    expect(withSuggestion("Remove the car", "Make it warmer")).toBe("Remove the car. Make it warmer");
    expect(withSuggestion("Remove the car.", "Make it warmer")).toBe("Remove the car. Make it warmer");
  });

  it("has six suggestions, each a dictionary key", () => {
    expect(LAB_SUGGESTIONS).toHaveLength(6);
    expect(new Set(LAB_SUGGESTIONS).size).toBe(6);
  });
});

describe("polling", () => {
  it("asks often while a job is queued or running, rarely otherwise", () => {
    expect(pollDelay([])).toBe(POLL_IDLE_MS);
    expect(pollDelay([{ status: "done" }, { status: "failed" }, { status: "accepted" }])).toBe(POLL_IDLE_MS);
    expect(pollDelay([{ status: "done" }, { status: "queued" }])).toBe(POLL_ACTIVE_MS);
    expect(pollDelay([{ status: "running" }])).toBe(POLL_ACTIVE_MS);
    expect(isActive({ status: "done" })).toBe(false);
  });
});

describe("progressLine", () => {
  it("says where a job is", () => {
    expect(progressLine(labJob("a", { status: "queued", queuePosition: 3 }))).toEqual({ key: "lab.queuePosition", params: { position: 3 } });
    expect(progressLine(labJob("a", { status: "queued", queuePosition: null }))).toEqual({ key: "lab.starting" });
    expect(progressLine(labJob("a", { status: "running", phase: "waiting" }))).toEqual({ key: "lab.waitingGpu" });
    expect(progressLine(labJob("a", { status: "running", phase: "generating", progress: 0 }))).toEqual({ key: "lab.starting" });
    expect(progressLine(labJob("a", { status: "running", phase: "generating", progress: 42 }))).toEqual({ key: "lab.generating", params: { percent: 42 } });
    expect(progressLine(labJob("a", { status: "done" }))).toBeNull();
  });
});

describe("sortJobs and counts", () => {
  it("puts the running job first, the queue in order, then finished ones newest first", () => {
    const jobs = [
      labJob("old-done", { status: "done", createdAt: "2026-10-27T10:00:00Z" }),
      labJob("q2", { status: "queued", createdAt: "2026-10-28T10:02:00Z" }),
      labJob("failed", { status: "failed", createdAt: "2026-10-28T08:00:00Z" }),
      labJob("run", { status: "running", createdAt: "2026-10-28T10:00:00Z" }),
      labJob("q1", { status: "queued", createdAt: "2026-10-28T10:01:00Z" }),
      labJob("new-done", { status: "done", createdAt: "2026-10-28T09:00:00Z" }),
      labJob("acc", { status: "accepted", createdAt: "2026-10-29T09:00:00Z" }),
    ];
    expect(sortJobs(jobs).map((j) => j.id)).toEqual(["run", "q1", "q2", "new-done", "old-done", "failed", "acc"]);
    expect(readyCount(jobs)).toBe(2);
  });

  it("names every status", () => {
    for (const s of ["queued", "running", "done", "failed", "accepted"] as const) expect(statusKey(s)).toMatch(/^lab\.status/);
  });
});

describe("splitFromPointer", () => {
  it("maps a pointer to 0..100 inside the box and clamps outside it", () => {
    expect(splitFromPointer(150, 100, 200)).toBe(25);
    expect(splitFromPointer(50, 100, 200)).toBe(0);
    expect(splitFromPointer(900, 100, 200)).toBe(100);
    expect(splitFromPointer(10, 0, 0)).toBe(50);
  });
});
