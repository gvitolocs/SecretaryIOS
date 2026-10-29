import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "./client";
import { instagramApi } from "./instagram";
import { stubFetch } from "../test/fetch";

describe("instagramApi", () => {
  afterEach(() => vi.restoreAllMocks());

  it("unwraps {ok,data} and encodes paths and cursors", async () => {
    const calls = stubFetch(() => ({ body: { ok: true, data: { items: [], nextCursor: null } } }));
    await instagramApi.posts("na.t_geo", "A+B/c=");
    await instagramApi.reels("natgeo");
    await instagramApi.stories("natgeo");
    await instagramApi.highlights("na.t_geo");
    await instagramApi.highlightItems("highlight:1", "x y");
    await instagramApi.highlightItems("highlight:1");
    await instagramApi.resolve("https://www.instagram.com/p/abc/?x=1&y=2");
    expect(calls.map((c) => c.url)).toEqual([
      "/v1/instagram/posts/na.t_geo?cursor=A%2BB%2Fc%3D",
      "/v1/instagram/reels/natgeo",
      "/v1/instagram/stories/natgeo",
      "/v1/instagram/highlights/na.t_geo",
      "/v1/instagram/highlights/items?id=highlight%3A1&cursor=x%20y",
      "/v1/instagram/highlights/items?id=highlight%3A1",
      "/v1/instagram/resolve?url=https%3A%2F%2Fwww.instagram.com%2Fp%2Fabc%2F%3Fx%3D1%26y%3D2",
    ]);
    expect(calls.every((c) => c.method === "GET" && c.credentials === "same-origin")).toBe(true);
  });

  it("reads the recent list", async () => {
    stubFetch(() => ({ body: { ok: true, data: { items: [{ username: "nasa", fullName: "NASA", isVerified: true, picUrl: null, openedAt: 1 }] } } }));
    expect(await instagramApi.recent()).toEqual([{ username: "nasa", fullName: "NASA", isVerified: true, picUrl: null, openedAt: 1 }]);
  });

  it("saves with the CSRF header and a JSON body", async () => {
    const calls = stubFetch(() => ({ body: { ok: true, data: { saved: [{ index: 0, itemId: "itm_1", duplicate: false }], failed: [] } } }));
    const r = await instagramApi.save([{ url: "/v1/instagram/s/1/2/3", kind: "image", username: "natgeo", takenAt: 5, caption: "hi" }]);
    expect(r.saved[0]).toEqual({ index: 0, itemId: "itm_1", duplicate: false });
    expect(calls[0]).toMatchObject({ method: "POST", url: "/v1/instagram/save" });
    expect(calls[0]!.headers["x-secretary-csrf"]).toBe("1");
    expect(calls[0]!.body).toEqual({ items: [{ url: "/v1/instagram/s/1/2/3", kind: "image", username: "natgeo", takenAt: 5, caption: "hi" }] });
  });

  it("turns a 429 into an ApiError that knows how long to wait", async () => {
    stubFetch(() => ({ status: 429, headers: { "Retry-After": "12" }, body: { ok: false, error: "rate_limited", retryAfter: 12 } }));
    const err = await instagramApi.profile("natgeo").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(429);
    expect((err as ApiError).retryAfterSeconds).toBe(12);
  });

  it("reads and changes the watch settings", async () => {
    const data = { intervalHours: 12, defaultHours: 23, jitterMinutes: 30, choices: [6, 12, 23] };
    const calls = stubFetch(() => ({ body: { ok: true, data } }));
    expect(await instagramApi.watchSettings()).toEqual(data);
    expect(await instagramApi.setWatchSettings(12)).toEqual(data);
    expect(calls.map((c) => `${c.method} ${c.url}`)).toEqual(["GET /v1/instagram/watch-settings", "PUT /v1/instagram/watch-settings"]);
    expect(calls[1]!.body).toEqual({ intervalHours: 12 });
    expect(calls[1]!.headers["x-secretary-csrf"]).toBe("1");
  });

  it("passes a refused setting on as an ApiError", async () => {
    stubFetch(() => ({ status: 400, body: { ok: false, error: "bad_request" } }));
    const err = await instagramApi.setWatchSettings(24).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(400);
  });

  it("talks to the watch routes", async () => {
    const calls = stubFetch((c) => ({ body: { ok: true, data: c.url.endsWith("/badges") ? { total: 0, accounts: {} } : [] } }));
    await instagramApi.watchList();
    await instagramApi.watch("na.tgeo", { autoSave: false });
    await instagramApi.watch("natgeo");
    await instagramApi.unwatch("natgeo");
    await instagramApi.opened("natgeo");
    await instagramApi.badges();
    await instagramApi.archive("natgeo", "30");
    expect(calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET /v1/instagram/watch",
      "PUT /v1/instagram/watch/na.tgeo",
      "PUT /v1/instagram/watch/natgeo",
      "DELETE /v1/instagram/watch/natgeo",
      "POST /v1/instagram/watch/natgeo/opened",
      "GET /v1/instagram/badges",
      "GET /v1/instagram/archive/natgeo?cursor=30",
    ]);
    expect(calls[1]!.body).toEqual({ autoSave: false });
    expect(calls.slice(1, 5).every((c) => c.headers["x-secretary-csrf"] === "1")).toBe(true);
  });
});
