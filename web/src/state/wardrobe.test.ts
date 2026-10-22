import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { TimelineItem } from "../api/timeline";
import { deferred, stubFetch, type Call } from "../test/fetch";
import { OUTFITS, settings } from "../test/wardrobe";
import { receiveItem } from "./chat";
import {
  avatarError,
  avatarSettings,
  CACHE_KEY,
  currentOutfit,
  handleTimelineItem,
  loadAvatar,
  resetWardrobeState,
  setAvatarPath,
  startAvatarSync,
  updateAvatar,
  wardrobeOutfits,
} from "./wardrobe";

function serve(state = settings(), extra?: (c: Call) => { status?: number; body?: unknown } | null) {
  let current = state;
  return stubFetch((c) => {
    const x = extra?.(c);
    if (x) return x;
    if (c.url === "/v1/avatar/wardrobe") return { body: OUTFITS };
    if (c.url === "/v1/avatar" && c.method === "GET") return { body: current };
    if (c.url === "/v1/avatar" && c.method === "PUT") {
      const p = c.body as Record<string, unknown>;
      current = { ...current, ...p, rules: { ...current.rules, ...((p.rules as object) ?? {}) }, updatedBy: "web" } as typeof current;
      return { body: current };
    }
    return { status: 404 };
  });
}

const lucinaItem = (meta: TimelineItem["meta"], ts = Date.now()): TimelineItem => ({
  id: String(ts),
  ts,
  channel: "telegram",
  role: "lucina",
  text: "Eccomi!",
  meta,
});

describe("wardrobe state", () => {
  beforeEach(() => resetWardrobeState());
  afterEach(() => resetWardrobeState());

  it("loads, resolves by section and caches", async () => {
    serve();
    await loadAvatar();
    expect(wardrobeOutfits().map((o) => o.id)).toEqual(["uniform", "gym", "chef", "maid"]);
    setAvatarPath("/gym/workout");
    expect(currentOutfit()?.id).toBe("gym");
    setAvatarPath("/shopping");
    expect(currentOutfit()?.id).toBe("chef");
    setAvatarPath("/");
    expect(currentOutfit()?.id).toBe("uniform");
    const cached = JSON.parse(localStorage.getItem(CACHE_KEY)!);
    expect(cached.outfits).toHaveLength(4);

    // A new page load starts from the cache, before any request.
    stubFetch(() => ({ status: 500 }));
    resetWardrobeState();
    expect(wardrobeOutfits()).toHaveLength(4);
  });

  it("keeps defaults when the server has no wardrobe yet", async () => {
    stubFetch(() => ({ status: 404 }));
    await loadAvatar();
    expect(wardrobeOutfits()).toEqual([]);
    expect(currentOutfit()).toBeNull();
    expect(avatarSettings().rules.gym).toBe("gym");
  });

  it("writes optimistically with the CSRF header and rolls back a refused write", async () => {
    const calls = serve(settings(), (c) =>
      c.method === "PUT" && (c.body as { outfit?: string }).outfit === "pirate"
        ? { status: 422, body: { error: { code: "unknown_outfit", message: "no" } } }
        : null,
    );
    await loadAvatar();
    const pending = updateAvatar({ outfit: "maid" });
    expect(currentOutfit()?.id).toBe("maid");
    expect(await pending).toBe(true);
    const put = calls.find((c) => c.method === "PUT")!;
    expect(put.url).toBe("/v1/avatar");
    expect(put.body).toEqual({ outfit: "maid" });
    expect(put.headers["x-secretary-csrf"]).toBe("1");
    expect(put.headers["x-secretary-client"]).toBe("web");

    expect(await updateAvatar({ outfit: "pirate" })).toBe(false);
    expect(avatarSettings().outfit).toBe("maid");
    expect(avatarError()).toMatch(/isn't in the wardrobe/);
  });

  it("a read that lands during a write does not undo it", async () => {
    const gate = deferred<void>();
    serve(settings(), (c) => (c.method === "GET" && c.url === "/v1/avatar" ? null : null));
    await loadAvatar();
    stubFetch(async (c) => {
      if (c.method === "PUT") {
        await gate.promise;
        return { body: settings({ outfit: "gym" }) };
      }
      if (c.url === "/v1/avatar/wardrobe") return { body: OUTFITS };
      return { body: settings() };
    });
    const w = updateAvatar({ outfit: "gym" });
    await loadAvatar();
    expect(avatarSettings().outfit).toBe("gym");
    gate.resolve();
    await w;
    expect(avatarSettings().outfit).toBe("gym");
  });

  it("applies Lucina's live outfit changes from the timeline, not old ones", async () => {
    const calls = serve(settings({ outfit: "maid", updatedBy: "lucina" }));
    await loadAvatar();
    resetWardrobeState();
    await loadAvatar();
    const off = startAvatarSync();
    try {
      const before = calls.length;
      expect(handleTimelineItem(lucinaItem({ kind: "avatar_outfit", outfit: "maid" }, Date.now() - 60 * 60_000))).toBe(false);
      expect(handleTimelineItem({ ...lucinaItem({ kind: "avatar_outfit", outfit: "maid" }), role: "user" })).toBe(false);
      expect(calls.length).toBe(before);

      receiveItem(lucinaItem({ kind: "avatar_outfit", outfit: "maid" }));
      expect(avatarSettings().outfit).toBe("maid");
      await Promise.resolve();
      expect(calls.filter((c) => c.url === "/v1/avatar").length).toBeGreaterThan(0);

      // Back to automatic: no outfit in the meta.
      handleTimelineItem(lucinaItem({ kind: "avatar_outfit" }));
      expect(avatarSettings().outfit).toBeNull();
      expect(avatarSettings().auto).toBe(true);
    } finally {
      off();
    }
  });
});
