import { afterEach, describe, expect, it, vi } from "vitest";
import { stubFetch } from "../test/fetch";
import { instagramBadges, instagramWatched, refreshInstagramBadges, refreshInstagramWatched, resetInstagramBadges, setInstagramBadges, startInstagramBadges } from "./instagramBadge";

function shell() {
  const setBadge = vi.fn();
  (window as { secretaryShell?: unknown }).secretaryShell = { showCode() {}, setBadge };
  return setBadge;
}

describe("Instagram badges", () => {
  afterEach(() => {
    resetInstagramBadges();
    delete (window as { secretaryShell?: unknown }).secretaryShell;
    vi.useRealTimers();
  });

  it("reads the badges and tells the desktop rail", async () => {
    const setBadge = shell();
    stubFetch(() => ({ body: { ok: true, data: { total: 5, accounts: { natgeo: 5 } } } }));
    await refreshInstagramBadges();
    expect(instagramBadges()).toEqual({ total: 5, accounts: { natgeo: 5 } });
    expect(setBadge).toHaveBeenLastCalledWith("instagram", 5);
  });

  it("works without a rail and leaves the dots alone when the request fails", async () => {
    setInstagramBadges({ total: 2, accounts: { a: 2 } });
    stubFetch(() => ({ status: 403, body: { ok: false, error: "owner_only" } }));
    await refreshInstagramBadges();
    expect(instagramBadges().total).toBe(2);
  });

  it("a rail that throws does not break the page", () => {
    (window as { secretaryShell?: unknown }).secretaryShell = { showCode() {}, setBadge: () => { throw new Error("no"); } };
    expect(() => setInstagramBadges({ total: 1, accounts: {} })).not.toThrow();
    expect(instagramBadges().total).toBe(1);
  });

  it("polls every two minutes until stopped, and resets to zero", async () => {
    vi.useFakeTimers();
    const setBadge = shell();
    const calls = stubFetch(() => ({ body: { ok: true, data: { total: 1, accounts: { x: 1 } } } }));
    const stop = startInstagramBadges();
    await vi.advanceTimersByTimeAsync(0);
    expect(calls).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(120_000);
    expect(calls).toHaveLength(2);
    stop();
    await vi.advanceTimersByTimeAsync(240_000);
    expect(calls).toHaveLength(2);
    resetInstagramBadges();
    expect(instagramBadges().total).toBe(0);
    expect(setBadge).toHaveBeenLastCalledWith("instagram", 0);
  });

  it("keeps the watched list, and an old one when a refresh fails", async () => {
    stubFetch(() => ({ body: { ok: true, data: [{ username: "natgeo", autoSave: true, lastCheck: null, newStories: 0, newPosts: 0, private: null }] } }));
    await refreshInstagramWatched();
    expect(instagramWatched().map((w) => w.username)).toEqual(["natgeo"]);
    stubFetch(() => ({ status: 502, body: {} }));
    await refreshInstagramWatched();
    expect(instagramWatched()).toHaveLength(1);
  });
});
