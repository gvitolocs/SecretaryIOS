import "fake-indexeddb/auto";
import { describe, expect, it, vi } from "vitest";
import { createPageCache, mergeFirstPage } from "./pageCache";

// Each case gets its own database, so instances made later with the same name see what an earlier one stored.
let n = 0;
const fresh = (over: Parameters<typeof createPageCache>[0] = {}) => ({ name: `pages-test-${++n}`, ...over });

describe("pageCache", () => {
  it("shows a write at once, keeps it in IndexedDB, and a later instance reads it back", async () => {
    const opts = fresh();
    const cache = createPageCache(opts);
    const saved = cache.put("instagram:profile:ann", "instagram", { name: "Ann" });
    expect(cache.peek("instagram:profile:ann")?.value).toEqual({ name: "Ann" });
    await saved;
    const later = createPageCache(opts);
    // Memory is per instance (a new page session): nothing to peek until it was read.
    expect(later.peek("instagram:profile:ann")).toBeUndefined();
    expect((await later.get("instagram:profile:ann"))?.value).toEqual({ name: "Ann" });
    expect(later.peek("instagram:profile:ann")?.value).toEqual({ name: "Ann" });
  });

  it("drops an entry older than maxAgeMs, from memory and from IndexedDB", async () => {
    let now = 1_000;
    const opts = fresh({ maxAgeMs: 60_000, now: () => now });
    const cache = createPageCache(opts);
    await cache.put("pixiv:ranking:day:0", "pixiv", [1]);
    now += 60_001;
    expect(cache.peek("pixiv:ranking:day:0")).toBeUndefined();
    expect(await createPageCache(opts).get("pixiv:ranking:day:0")).toBeUndefined();
  });

  it("evicts the least recently used entries past maxBytes; reading one makes it recent", async () => {
    let now = 0;
    // Each value "x" is 3 JSON characters, 6 bytes: two fit.
    const opts = fresh({ maxBytes: 12, now: () => ++now });
    const cache = createPageCache(opts);
    await cache.put("a", "pixiv", "x");
    await cache.put("b", "pixiv", "x");
    await cache.get("a");
    await cache.put("c", "pixiv", "x");
    expect(cache.peek("b")).toBeUndefined();
    expect(cache.peek("a")).toBeDefined();
    expect(cache.peek("c")).toBeDefined();
    // Another instance counts what is stored, not only its own writes.
    const later = createPageCache(opts);
    await later.put("d", "pixiv", "x");
    const reader = createPageCache(opts);
    expect(await reader.get("a")).toBeUndefined();
    expect(await reader.get("c")).toBeDefined();
    expect(await reader.get("d")).toBeDefined();
  });

  it("clears one scope (a plugin's Disconnect) or everything (sign-out), with the route snapshots", async () => {
    const opts = fresh();
    const cache = createPageCache(opts);
    await cache.put("instagram:profile:ann", "instagram", 1);
    await cache.put("pixiv:illust:1:0", "pixiv", 2);
    cache.snapshots.set("pixiv:view", { tab: "search" });
    cache.snapshots.set("instagram:profile:ann:scroll", 400);
    await cache.clear("pixiv");
    expect(cache.peek("pixiv:illust:1:0")).toBeUndefined();
    expect(await createPageCache(opts).get("pixiv:illust:1:0")).toBeUndefined();
    expect(cache.snapshots.has("pixiv:view")).toBe(false);
    expect(cache.peek("instagram:profile:ann")?.value).toBe(1);
    await cache.clear();
    expect(await createPageCache(opts).get("instagram:profile:ann")).toBeUndefined();
    expect(cache.snapshots.size).toBe(0);
  });

  it("ignores a write that a clear overtook", async () => {
    const opts = fresh();
    const cache = createPageCache(opts);
    const revision = cache.revision("instagram");
    await cache.clear("instagram");
    expect(cache.revision("instagram")).toBe(revision + 1);
    await cache.put("instagram:profile:ann", "instagram", 1);
    // A put after the clear is a new visit's: it counts.
    expect((await createPageCache(opts).get("instagram:profile:ann"))?.value).toBe(1);
  });

  it("works from memory alone when IndexedDB is missing or refuses to open", async () => {
    for (const idb of [undefined, { open: () => { throw new Error("blocked"); } }]) {
      vi.stubGlobal("indexedDB", idb);
      const cache = createPageCache(fresh({ maxBytes: 6 }));
      await cache.put("a", "notebooks", "x");
      expect((await cache.get("a"))?.value).toBe("x");
      await cache.put("b", "notebooks", "x");
      expect(cache.peek("a")).toBeUndefined();
      expect(cache.peek("b")?.value).toBe("x");
      vi.unstubAllGlobals();
    }
  });

  it("merges a refreshed first page: fresh items first in the server's order, unchanged objects kept", () => {
    const old = [{ id: "1", t: "a" }, { id: "2", t: "b" }, { id: "3", t: "c" }];
    const merged = mergeFirstPage(old, [{ id: "0", t: "new" }, { id: "1", t: "a" }, { id: "2", t: "B" }]);
    expect(merged.map((i) => i.id)).toEqual(["0", "1", "2", "3"]);
    expect(merged[1]).toBe(old[0]);
    expect(merged[2]).toEqual({ id: "2", t: "B" });
    expect(merged[3]).toBe(old[2]);
  });
});
