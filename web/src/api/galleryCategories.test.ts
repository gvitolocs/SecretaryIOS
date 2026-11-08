import { afterEach, describe, expect, it, vi } from "vitest";
import { stubFetch } from "../test/fetch";
import {
  GALLERY_CATEGORIES,
  getCategoryStatus,
  getDetails,
  isGalleryCategory,
  listCategories,
  listCategoryItems,
  parseCategoryAlbum,
  parseItemCategory,
  setItemCategory,
} from "./gallery";

afterEach(() => vi.unstubAllGlobals());

const rawItem = (id: string) => ({ id, kind: "photo", title: `${id}.jpg`, mime: "image/jpeg", size: 1, created_at: "2026-10-01T10:00:00Z", urls: {} });

describe("parsing", () => {
  it("knows exactly the ten categories, in the apps' order", () => {
    expect(GALLERY_CATEGORIES).toEqual(["screenshot", "receipt", "document", "note", "ticket", "map", "meme", "food", "pet", "photo"]);
    expect(isGalleryCategory("receipt")).toBe(true);
    for (const bad of ["Receipt", "", "receipts", null, 3, undefined]) expect(isGalleryCategory(bad)).toBe(false);
  });

  it("reads a decision, keeping who made it and what the machine said before the owner did", () => {
    expect(parseItemCategory({ category: "receipt", confidence: 0.83, decided_by: "rules", auto_category: null })).toEqual({
      category: "receipt",
      confidence: 0.83,
      decidedBy: "rules",
      autoCategory: null,
    });
    expect(parseItemCategory({ category: "photo", confidence: 1, decided_by: "owner", auto_category: "receipt" })).toEqual({
      category: "photo",
      confidence: 1,
      decidedBy: "owner",
      autoCategory: "receipt",
    });
    expect(parseItemCategory({ category: "meme", confidence: 0.9, decided_by: "jev" })?.decidedBy).toBe("jev");
  });

  it("clamps the confidence, defaults the decider to rules and ignores an unknown auto category", () => {
    const c = parseItemCategory({ category: "pet", confidence: 7, decided_by: "gpt", auto_category: "dragon" });
    expect(c).toEqual({ category: "pet", confidence: 1, decidedBy: "rules", autoCategory: null });
    expect(parseItemCategory({ category: "pet" })?.confidence).toBe(0);
  });

  it("has no category for anything that is not one", () => {
    for (const raw of [null, undefined, "receipt", [], {}, { category: "dragon" }, { category: 3 }]) expect(parseItemCategory(raw)).toBeNull();
  });

  it("reads an automatic album: only a real category other than photo, with photos in it", () => {
    expect(parseCategoryAlbum({ category: "food", count: 4, cover: rawItem("a") })).toMatchObject({ category: "food", count: 4, cover: { id: "a" } });
    expect(parseCategoryAlbum({ category: "food", count: 4 })).toEqual({ category: "food", count: 4, cover: null });
    for (const raw of [{ category: "photo", count: 4 }, { category: "food", count: 0 }, { category: "food" }, { category: "x", count: 1 }, null]) expect(parseCategoryAlbum(raw)).toBeNull();
  });
});

describe("requests", () => {
  it("lists the automatic albums of the Gallery's photos and drops the ones it cannot read", async () => {
    const calls = stubFetch(() => ({
      body: { ok: true, categories: [{ category: "receipt", count: 2, cover: rawItem("r") }, { category: "photo", count: 9 }, { category: "pet", count: 1, cover: null }] },
    }));
    const list = await listCategories();
    expect(list.map((c) => [c.category, c.count])).toEqual([["receipt", 2], ["pet", 1]]);
    const url = new URL(calls[0]!.url, "http://x");
    expect(url.pathname).toBe("/v1/media/categories");
    expect(Object.fromEntries(url.searchParams)).toEqual({ scope: "gallery", origin: "all", kind: "photo" });
    expect(calls[0]!.method).toBe("GET");
  });

  it("pages one category's photos through its own route, with the listing's filters and no category parameter", async () => {
    const calls = stubFetch(() => ({ body: { ok: true, items: [rawItem("a"), { nope: true }], next_cursor: "cur" } }));
    const page = await listCategoryItems("receipt", { origin: "device:ios-1", from: "2026-01-01", q: "coop", cursor: "c0", limit: 50, category: "receipt" });
    expect(page.items.map((i) => i.id)).toEqual(["a"]);
    expect(page.nextCursor).toBe("cur");
    const url = new URL(calls[0]!.url, "http://x");
    expect(url.pathname).toBe("/v1/media/categories/receipt/items");
    expect(url.searchParams.get("category")).toBeNull();
    expect(url.searchParams.get("origin")).toBe("device:ios-1");
    expect(url.searchParams.get("from")).toBe("2026-01-01");
    expect(url.searchParams.get("q")).toBe("coop");
    expect(url.searchParams.get("cursor")).toBe("c0");
    expect(url.searchParams.get("limit")).toBe("50");
  });

  it("sets and takes back the owner's label", async () => {
    const calls = stubFetch((call) =>
      (call.body as { category: string | null }).category === null
        ? { body: { ok: true, category: null } }
        : { body: { ok: true, category: { category: "photo", confidence: 1, decided_by: "owner", auto_category: "receipt" } } },
    );
    const now = await setItemCategory("it 1", "photo");
    expect(now).toEqual({ category: "photo", confidence: 1, decidedBy: "owner", autoCategory: "receipt" });
    expect(calls[0]!.method).toBe("PUT");
    expect(calls[0]!.url).toBe("/v1/media/items/it%201/category");
    expect(calls[0]!.body).toEqual({ category: "photo" });
    expect(await setItemCategory("it 1", null)).toBeNull();
    expect(calls[1]!.body).toEqual({ category: null });
  });

  it("reads the status", async () => {
    stubFetch(() => ({
      body: {
        ok: true,
        enabled: true,
        model: "siglip",
        decided: 90,
        pending: 10,
        stale: 2,
        running: true,
        paused_hot: false,
        paused_busy: true,
        decider: { kind: "rules", id: "rules-1" },
        by_decider: { rules: 80, jev: 0, owner: 10 },
      },
    }));
    expect(await getCategoryStatus()).toEqual({
      enabled: true,
      model: "siglip",
      decided: 90,
      pending: 10,
      stale: 2,
      running: true,
      pausedHot: false,
      pausedBusy: true,
      decider: { kind: "rules", id: "rules-1" },
      byDecider: { rules: 80, jev: 0, owner: 10 },
    });
    stubFetch(() => ({ body: {} }));
    expect(await getCategoryStatus()).toMatchObject({ enabled: false, model: null, decided: 0, decider: null });
  });

  it("brings the category with the photo's details, null while there is none", async () => {
    stubFetch(() => ({ body: { item: rawItem("a"), exif: null, people: [], category: { category: "ticket", confidence: 0.7, decided_by: "rules", auto_category: null } } }));
    expect((await getDetails("a"))?.category).toMatchObject({ category: "ticket", decidedBy: "rules" });
    stubFetch(() => ({ body: { item: rawItem("a"), exif: null, people: [] } }));
    expect((await getDetails("a"))?.category).toBeNull();
  });
});
