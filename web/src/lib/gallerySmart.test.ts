import { describe, expect, it, vi } from "vitest";
import { SmartUnavailableError, type EmbeddingStatus, type GalleryPage } from "../api/gallery";
import { lookedAt, smartPager, smartReady, usesSmart, withSearch } from "./gallerySmart";

const status = (over: Partial<EmbeddingStatus> = {}): EmbeddingStatus => ({ enabled: true, model: "m", indexed: 100, pending: 0, running: false, pausedHot: false, ...over });
const page = (ids: string[], next: string | null = null): GalleryPage => ({ items: ids.map((id) => ({ id }) as never), nextCursor: next });

describe("when a query is smart", () => {
  it("needs words, the smart choice, an enabled index with something in it, and no refusal", () => {
    const ok = { q: "beach", mode: "smart" as const, status: status(), refused: false };
    expect(usesSmart(ok)).toBe(true);
    expect(usesSmart({ ...ok, q: "  " })).toBe(false);
    expect(usesSmart({ ...ok, mode: "words" })).toBe(false);
    expect(usesSmart({ ...ok, refused: true })).toBe(false);
    expect(usesSmart({ ...ok, status: status({ enabled: false }) })).toBe(false);
    expect(usesSmart({ ...ok, status: status({ indexed: 0 }) })).toBe(false);
    expect(usesSmart({ ...ok, status: null })).toBe(false);
  });

  it("smartReady is false when the server is off or has looked at nothing", () => {
    expect(smartReady(status())).toBe(true);
    expect(smartReady(status({ indexed: 0, pending: 50 }))).toBe(false);
    expect(smartReady(undefined)).toBe(false);
  });
});

describe("the hint", () => {
  it("says how many photos it has looked at only while some are still waiting", () => {
    expect(lookedAt(status({ indexed: 1200, pending: 300 }))).toBe(1200);
    expect(lookedAt(status({ indexed: 1500, pending: 0 }))).toBeNull();
    expect(lookedAt(status({ enabled: false, indexed: 5, pending: 5 }))).toBeNull();
    expect(lookedAt(null)).toBeNull();
  });
});

describe("withSearch", () => {
  it("adds smart and similar-to to the ordinary query", () => {
    expect(withSearch({ origin: "all" }, { smart: true })).toEqual({ origin: "all", smart: true, similarTo: undefined });
    expect(withSearch({ origin: "all" }, { smart: false, similarTo: "p1" })).toEqual({ origin: "all", smart: undefined, similarTo: "p1" });
  });
});

describe("smartPager", () => {
  const hooks = () => ({ onUnavailable: vi.fn(), onStatus: vi.fn() });

  it("sends a plain query to the words listing", async () => {
    const smart = vi.fn();
    const words = vi.fn().mockResolvedValue(page(["a"]));
    const h = hooks();
    const got = await smartPager(smart, h, words)({ origin: "all", q: "beach" });
    expect(got.items).toHaveLength(1);
    expect(smart).not.toHaveBeenCalled();
  });

  it("returns a smart page with its cursor and reports the index status", async () => {
    const h = hooks();
    const smart = vi.fn().mockResolvedValue({ ...page(["b", "a"], "c2"), scores: [0.9, 0.8], status: status({ pending: 4 }) });
    const got = await smartPager(smart, h, vi.fn())({ q: "beach", smart: true });
    expect(got).toEqual({ items: page(["b", "a"]).items, nextCursor: "c2" });
    expect(h.onStatus).toHaveBeenCalledWith(expect.objectContaining({ pending: 4 }));
    expect(h.onUnavailable).not.toHaveBeenCalled();
  });

  it("falls back to the words search when smart search answers 503, and tells the Gallery", async () => {
    const h = hooks();
    const smart = vi.fn().mockRejectedValue(new SmartUnavailableError());
    const words = vi.fn().mockResolvedValue(page(["w1", "w2"]));
    const got = await smartPager(smart, h, words)({ q: "beach", smart: true, origin: "all" });
    expect(got.items.map((i) => i.id)).toEqual(["w1", "w2"]);
    expect(h.onUnavailable).toHaveBeenCalledTimes(1);
    expect(words).toHaveBeenCalledWith(expect.objectContaining({ q: "beach", smart: false }));
  });

  it("a refused 'similar' fails (there is nothing to fall back to) and tells the Gallery", async () => {
    const h = hooks();
    const smart = vi.fn().mockRejectedValue(new SmartUnavailableError());
    await expect(smartPager(smart, h, vi.fn())({ similarTo: "p1" })).rejects.toBeInstanceOf(SmartUnavailableError);
    expect(h.onUnavailable).toHaveBeenCalledTimes(1);
  });

  it("any other failure is an ordinary failure, not a switch to words", async () => {
    const h = hooks();
    const smart = vi.fn().mockRejectedValue(new Error("offline"));
    await expect(smartPager(smart, h, vi.fn())({ q: "x", smart: true })).rejects.toThrow("offline");
    expect(h.onUnavailable).not.toHaveBeenCalled();
  });
});
