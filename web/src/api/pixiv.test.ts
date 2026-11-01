import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setShowAdult } from "../lib/adult";
import { stubFetch } from "../test/fetch";
import { ApiError } from "./client";
import { isAdultOff, parseIllust, parseItem, parseListing, pixivApi } from "./pixiv";

const LIST = { ok: true, data: { items: [{ id: "1", title: "Sky", author: "Ann", authorId: "7", tags: ["sky"], thumbUrl: "/v1/pixiv/s/1/a/b", pages: 3, r18: false, bookmarked: true }], page: 2, hasMore: true, total: null, numPages: null } };

describe("parsing", () => {
  it("fills the defaults of a thin item and drops items without an id", () => {
    expect(parseItem({ id: "5" })).toEqual({ id: "5", title: "", author: "", authorId: "", tags: [], thumbUrl: "", pages: 1, r18: false, bookmarked: false, popularTier: null });
    expect(parseListing({ items: [{ id: "1" }, { nope: 1 }], hasMore: true }).items.map((i) => i.id)).toEqual(["1"]);
    expect(parseListing(undefined)).toEqual({ items: [], page: 1, hasMore: false, total: null, numPages: null, notice: null, sort: null });
  });

  it("reads the notice, the sort and the popularity tier, and shrugs off garbage", () => {
    const l = parseListing({ items: [{ id: "1", popularTier: 1000 }, { id: "2", popularTier: "1000" }], notice: "r18_needs_session", sort: { requested: "popular", used: "users_tags" } });
    expect(l.notice).toBe("r18_needs_session");
    expect(l.sort).toEqual({ requested: "popular", used: "users_tags" });
    expect(l.items.map((i) => i.popularTier)).toEqual([1000, null]);
    for (const junk of [{ notice: 3, sort: "popular" }, { notice: "", sort: { requested: "popular" } }, { notice: null, sort: [] }]) {
      expect(parseListing(junk)).toMatchObject({ notice: null, sort: null });
    }
    expect(parseListing({ sort: { used: "date_d" } }).sort).toEqual({ requested: "", used: "date_d" });
    for (const tier of [0, -5, Number.NaN, null, {}]) expect(parseItem({ id: "1", popularTier: tier }).popularTier).toBeNull();
  });

  it("reads a work with its pages and tags", () => {
    const w = parseIllust({
      id: "9", title: "T", kind: "manga", pageCount: 2, stats: { views: 5 }, tags: [{ tag: "風景", en: "landscape" }, { en: "x" }],
      pages: [{ index: 0, thumbUrl: "t0", url: "u0", originalUrl: "o0" }, { thumbUrl: "t1", url: "u1" }],
    });
    expect(w.kind).toBe("manga");
    expect(w.tags).toEqual([{ tag: "風景", en: "landscape" }]);
    expect(w.pages[1]).toEqual({ index: 1, thumbUrl: "t1", url: "u1", originalUrl: "" });
    expect(w.stats).toEqual({ views: 5, likes: 0, bookmarks: 0, comments: 0 });
    expect(parseIllust({ id: "1", kind: "weird" }).kind).toBe("illust");
  });
});

describe("requests", () => {
  beforeEach(() => setShowAdult(true));
  afterEach(() => {
    vi.unstubAllGlobals();
    setShowAdult(true);
  });

  it("sends adult=1 only while the switch is on", async () => {
    const calls = stubFetch(() => ({ body: LIST }));
    await pixivApi.ranking("day", 2);
    expect(calls[0]).toMatchObject({ method: "GET", url: "/v1/pixiv/ranking?mode=day&page=2&adult=1" });
    expect(calls[0]!.credentials).toBe("same-origin");
    setShowAdult(false);
    await pixivApi.ranking("week", 1);
    expect(calls[1]!.url).toBe("/v1/pixiv/ranking?mode=week&page=1");
    await pixivApi.search({ q: "blue sky", mode: "all" });
    expect(calls[2]!.url).toBe("/v1/pixiv/search?q=blue+sky&order=date_d&type=all&mode=all&page=1");
    await pixivApi.illust("12");
    await pixivApi.bookmarks();
    expect(calls[3]!.url).toBe("/v1/pixiv/illust/12");
    expect(calls[4]!.url).toBe("/v1/pixiv/bookmarks?content=safe");
  });

  it("asks for the popular order and passes the bookmarks' content filter", async () => {
    const calls = stubFetch(() => ({ body: { ok: true, data: { items: [], notice: "all_ages_only" } } }));
    await pixivApi.search({ q: "sky", order: "popular", mode: "all" });
    expect(calls[0]!.url).toBe("/v1/pixiv/search?q=sky&order=popular&type=all&mode=all&page=1&adult=1");
    const l = await pixivApi.bookmarks("r18");
    expect(calls[1]!.url).toBe("/v1/pixiv/bookmarks?content=r18&adult=1");
    expect(l.notice).toBe("all_ages_only");
  });

  it("returns the parsed listing", async () => {
    stubFetch(() => ({ body: LIST }));
    const l = await pixivApi.ranking("day");
    expect(l.page).toBe(2);
    expect(l.hasMore).toBe(true);
    expect(l.items[0]).toMatchObject({ id: "1", pages: 3, bookmarked: true });
  });

  it("writes carry the CSRF header and a JSON body", async () => {
    const calls = stubFetch((c) => (c.method === "POST" ? { body: { ok: true, data: { saved: [{ page: 1, itemId: "itm_1", duplicate: true }], failed: [{ page: 2, error: "ingest_failed" }] } } } : { body: { ok: true, data: { bookmarked: true } } }));
    expect(await pixivApi.addBookmark("12")).toBe(true);
    expect(calls[0]).toMatchObject({ method: "PUT", url: "/v1/pixiv/bookmarks/12?adult=1" });
    expect(calls[0]!.headers["x-secretary-csrf"]).toBe("1");
    const r = await pixivApi.save("12", [1, 2]);
    expect(calls[1]).toMatchObject({ method: "POST", url: "/v1/pixiv/save?adult=1", body: { id: "12", pages: [1, 2] } });
    expect(calls[1]!.headers["x-secretary-csrf"]).toBe("1");
    expect(r.saved).toEqual([{ page: 1, itemId: "itm_1", duplicate: true }]);
    expect(r.failed).toEqual([{ page: 2, error: "ingest_failed" }]);
    await pixivApi.save("12");
    expect(calls[2]!.body).toEqual({ id: "12" });
    stubFetch(() => ({ body: { ok: true, data: { bookmarked: false } } }));
    expect(await pixivApi.removeBookmark("12")).toBe(false);
  });

  it("tells adult_off from other refusals, and keeps Retry-After", async () => {
    stubFetch(() => ({ status: 403, body: { ok: false, error: "adult_off" } }));
    const e = await pixivApi.ranking("day_r18").catch((x: unknown) => x);
    expect(e).toBeInstanceOf(ApiError);
    expect(isAdultOff(e)).toBe(true);
    stubFetch(() => ({ status: 403, body: { ok: false, error: "forbidden" } }));
    expect(isAdultOff(await pixivApi.illust("1").catch((x: unknown) => x))).toBe(false);
    stubFetch(() => ({ status: 429, body: { ok: false, error: "rate_limited", retryAfter: 30 }, headers: { "Retry-After": "30" } }));
    const slow = (await pixivApi.search({ q: "x" }).catch((x: unknown) => x)) as ApiError;
    expect(slow.status).toBe(429);
    expect(slow.retryAfterSeconds).toBe(30);
  });
});

describe("connection API", () => {
  it("parses missing and unknown fields safely", async () => {
    const { parseConnection, parseSettings, parseStatus } = await import("./pixiv");
    for (const junk of [undefined, null, {}, { state: "future", premium: 1, r18: "yes", store: "other", removal: "saved", error: "new" }]) {
      expect(parseConnection(junk)).toEqual({ state: "unavailable", user: null, premium: null, r18: null, r18g: null, store: null, removal: null, error: null, connectedAt: null });
    }
    expect(parseSettings({ defaultContent: "unknown", defaultSort: "date" })).toEqual({ defaultContent: "safe", defaultSort: "date_d" });
    expect(parseStatus(null).settings).toEqual({ defaultContent: "safe", defaultSort: "date_d" });
    const c = { state: "connected", user: { id: "2", name: "Ann", account: "ann" }, premium: true, r18: false, r18g: false, store: "saved", removal: null, error: null, connectedAt: "now" };
    expect(parseConnection(c)).toEqual(c);
    expect(parseStatus({ connection: c, settings: { defaultContent: "all", defaultSort: "popular" }, account: { session: "ok", premium: true, r18: false }, enabled: true })).toMatchObject({ connection: c, settings: { defaultContent: "all", defaultSort: "popular" }, enabled: true });
  });
  it("uses the contract routes, methods and JSON payloads", async () => {
    const calls = stubFetch(() => ({ body: { ok: true, data: {} } }));
    await pixivApi.status(); await pixivApi.connectStart(); await pixivApi.connectFinish("state", "code");
    await pixivApi.disconnect(); await pixivApi.settings(); await pixivApi.saveSettings({ defaultContent: "r18" });
    expect(calls.map(c => [c.method, c.url, c.body])).toEqual([
      ["GET", "/v1/pixiv/status", undefined], ["POST", "/v1/pixiv/connect/start", undefined],
      ["POST", "/v1/pixiv/connect/finish", { state: "state", code: "code" }], ["POST", "/v1/pixiv/disconnect", undefined],
      ["GET", "/v1/pixiv/settings", undefined], ["PUT", "/v1/pixiv/settings", { defaultContent: "r18" }],
    ]);
  });
});
