import { createRoot, createSignal } from "solid-js";
import { describe, expect, it } from "vitest";
import { deferred } from "../test/fetch";
import { pageCache } from "./pageCache";
import { createCached } from "./swr";

const flush = () => new Promise((r) => setTimeout(r, 0));

describe("createCached", () => {
  it("shows the stored copy before the server answers, then the answer, which it stores", async () => {
    await pageCache.put("notebooks:detail:nb1", "notebooks", { name: "Old" });
    const answer = deferred<{ name: string }>();
    await createRoot(async (dispose) => {
      const c = createCached("notebooks", () => "notebooks:detail:nb1", () => answer.promise);
      expect(c.value()).toEqual({ name: "Old" });
      await flush();
      expect(c.updating()).toBe(true);
      expect(c.loading()).toBe(false);
      answer.resolve({ name: "New" });
      await flush();
      expect(c.value()).toEqual({ name: "New" });
      expect(c.updating()).toBe(false);
      expect(pageCache.peek("notebooks:detail:nb1")?.value).toEqual({ name: "New" });
      dispose();
    });
  });

  it("keeps the copy when a refresh fails; reports the error only with nothing to show", async () => {
    await pageCache.put("notebooks:list:exclude", "notebooks", ["a"]);
    await createRoot(async (dispose) => {
      const kept = createCached("notebooks", () => "notebooks:list:exclude", () => Promise.reject(new Error("offline")));
      const empty = createCached("notebooks", () => "notebooks:list:all", () => Promise.reject(new Error("offline")));
      await flush();
      await flush();
      expect(kept.value()).toEqual(["a"]);
      expect(kept.error()).toBeNull();
      expect(empty.value()).toBeUndefined();
      expect(empty.error()).toBeInstanceOf(Error);
      dispose();
    });
  });

  it("refreshing the same key keeps the value on screen even after its copy was cleared; a new key starts from its own", async () => {
    await pageCache.put("notebooks:detail:nb1", "notebooks", { name: "One" });
    const [id, setId] = createSignal("nb1");
    const later = deferred<{ name: string }>();
    await createRoot(async (dispose) => {
      let calls = 0;
      const c = createCached("notebooks", () => `notebooks:detail:${id()}`, () => (++calls === 1 ? Promise.resolve({ name: "One" }) : later.promise));
      await flush();
      await pageCache.clear("notebooks");
      void c.refresh();
      expect(c.value()).toEqual({ name: "One" });
      setId("nb2");
      await flush();
      expect(c.value()).toBeUndefined();
      expect(c.loading()).toBe(true);
      later.resolve({ name: "Two" });
      await flush();
      expect(c.value()).toEqual({ name: "Two" });
      dispose();
    });
  });
});
