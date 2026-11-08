import { afterEach, describe, expect, it, vi } from "vitest";
import { stubFetch } from "../test/fetch";
import {
  deleteAlbum,
  getEmbeddingStatus,
  listAlbumCards,
  listAlbums,
  listGalleryPage,
  listSimilar,
  parseItem,
  renameAlbum,
  searchSmart,
  SmartUnavailableError,
} from "./gallery";

afterEach(() => vi.unstubAllGlobals());

const raw = (id: string, extra: Record<string, unknown> = {}) => ({
  id,
  kind: "photo",
  title: `${id}.jpg`,
  mime: "image/jpeg",
  size: 1,
  created_at: "2026-10-01T10:00:00Z",
  urls: { thumb: `/v1/media/items/${id}/thumb?size=256` },
  ...extra,
});
const rawAlbum = (id: string, count: number, extra: Record<string, unknown> = {}) => ({
  id,
  title: `Album ${id}`,
  system_key: null,
  cover_item: null,
  count,
  created_at: "2026-10-01T10:00:00Z",
  updated_at: "2026-10-02T10:00:00Z",
  ...extra,
});
const err = (status: number) => ({ status, body: { error: { code: "x", message: "x" } } });

describe("Live Photos in the item JSON", () => {
  it("parses live_pair, null when missing", () => {
    expect(parseItem(raw("p1", { live_pair: "p1_mov" }))?.livePair).toBe("p1_mov");
    expect(parseItem(raw("p2"))?.livePair).toBeNull();
    expect(parseItem(raw("p3", { live_pair: "" }))?.livePair).toBeNull();
  });
});

describe("albums", () => {
  it("lists the Gallery's albums and adds the empty ones, but not the ones whose photos are elsewhere", async () => {
    const calls = stubFetch((call) => {
      const scoped = new URL(call.url, "http://x").searchParams.get("scope") === "gallery";
      return {
        body: {
          albums: scoped
            ? [rawAlbum("a", 4, { cover_item: "p9" }), rawAlbum("lucina", 2, { system_key: "lucina" })]
            : [rawAlbum("a", 4), rawAlbum("fresh", 0), rawAlbum("films", 12), rawAlbum("lucina", 2, { system_key: "lucina" })],
        },
      };
    });
    const albums = await listAlbumCards();
    expect(albums.map((a) => a.id)).toEqual(["a", "lucina", "fresh"]);
    expect(albums[0]).toEqual({ id: "a", title: "Album a", systemKey: null, coverItem: "p9", count: 4, createdAt: "2026-10-01T10:00:00Z", updatedAt: "2026-10-02T10:00:00Z" });
    expect(calls.map((c) => c.url)).toEqual(["/v1/media/albums?scope=gallery", "/v1/media/albums"]);
  });

  it("still lists the Gallery's albums when the plain listing fails", async () => {
    stubFetch((call) => (call.url.includes("scope=gallery") ? { body: { albums: [rawAlbum("a", 1)] } } : err(500)));
    expect((await listAlbumCards()).map((a) => a.id)).toEqual(["a"]);
  });

  it("the picker's list includes empty albums and leaves out share albums", async () => {
    stubFetch((call) =>
      call.url.includes("scope=gallery")
        ? { body: { albums: [rawAlbum("a", 1), rawAlbum("hidden", 2, { system_key: "share:t" })] } }
        : { body: { albums: [rawAlbum("fresh", 0)] } },
    );
    expect(await listAlbums()).toEqual([
      { id: "a", title: "Album a", count: 1, system: false },
      { id: "fresh", title: "Album fresh", count: 0, system: false },
    ]);
  });

  it("renames with PATCH and deletes with DELETE", async () => {
    const calls = stubFetch((call) => (call.method === "PATCH" ? { body: rawAlbum("a b", 3, { title: "New name" }) } : { status: 204 }));
    expect((await renameAlbum("a b", "New name")).title).toBe("New name");
    await deleteAlbum("a b");
    expect(calls[0]).toMatchObject({ method: "PATCH", url: "/v1/media/albums/a%20b", body: { title: "New name" } });
    expect(calls[1]).toMatchObject({ method: "DELETE", url: "/v1/media/albums/a%20b" });
  });
});

describe("smart search", () => {
  it("reads the index status, null when the server has none", async () => {
    stubFetch(() => ({ body: { enabled: true, model: "clip", indexed: 120, pending: 30, running: true, paused_hot: false } }));
    expect(await getEmbeddingStatus()).toEqual({ enabled: true, model: "clip", indexed: 120, pending: 30, running: true, pausedHot: false });
    stubFetch(() => err(503));
    expect(await getEmbeddingStatus()).toBeNull();
  });

  it("searches with the words and the filters, clamps the page size, and parses scores, cursor and status", async () => {
    const calls = stubFetch(() => ({
      body: { items: [raw("b"), raw("a")], scores: [0.91, 0.5], next_cursor: "c2", status: { enabled: true, model: null, indexed: 9, pending: 1, running: true, paused_hot: false } },
    }));
    const page = await searchSmart({ q: " red car ", origin: "device:d1", kind: "photo", favorite: true, album: "alb1", cursor: "c1", limit: 500 });
    const url = new URL(calls[0]!.url, "http://x");
    expect(url.pathname).toBe("/v1/media/items/search");
    expect(Object.fromEntries(url.searchParams)).toMatchObject({ q: "red car", origin: "device:d1", kind: "photo", favorite: "true", album: "alb1", cursor: "c1", limit: "200" });
    expect(page.items.map((i) => i.id)).toEqual(["b", "a"]);
    expect(page.scores).toEqual([0.91, 0.5]);
    expect(page.nextCursor).toBe("c2");
    expect(page.status).toMatchObject({ indexed: 9, pending: 1 });
  });

  it("asks 60 by default", async () => {
    const calls = stubFetch(() => ({ body: { items: [], scores: [], next_cursor: null, status: null } }));
    await searchSmart({ q: "x" });
    expect(new URL(calls[0]!.url, "http://x").searchParams.get("limit")).toBe("60");
  });

  it("503 (smart search off) and a missing route are SmartUnavailableError; other failures stay as they are", async () => {
    stubFetch(() => err(503));
    await expect(searchSmart({ q: "x" })).rejects.toBeInstanceOf(SmartUnavailableError);
    stubFetch(() => err(404));
    await expect(listSimilar("p1")).rejects.toBeInstanceOf(SmartUnavailableError);
    stubFetch(() => err(500));
    const failure = await searchSmart({ q: "x" }).catch((e: unknown) => e);
    expect(failure).toBeInstanceOf(Error);
    expect(failure).not.toBeInstanceOf(SmartUnavailableError);
  });

  it("asks for similar pictures of one item without any words", async () => {
    const calls = stubFetch(() => ({ body: { items: [raw("n1")], scores: [0.8], next_cursor: null, status: null } }));
    const page = await listSimilar("p 1", { q: "ignored", origin: "all" });
    const url = new URL(calls[0]!.url, "http://x");
    expect(url.pathname).toBe("/v1/media/items/p%201/similar");
    expect(url.searchParams.has("q")).toBe(false);
    expect(page.items).toHaveLength(1);
  });

  it("listGalleryPage picks the route: similar, then smart with words, else the plain listing", async () => {
    const calls = stubFetch(() => ({ body: { items: [], next_cursor: null } }));
    await listGalleryPage({ similarTo: "p1", q: "x" });
    await listGalleryPage({ smart: true, q: "x" });
    await listGalleryPage({ smart: true });
    await listGalleryPage({ q: "x" });
    expect(calls.map((c) => new URL(c.url, "http://x").pathname)).toEqual([
      "/v1/media/items/p1/similar",
      "/v1/media/items/search",
      "/v1/media/items",
      "/v1/media/items",
    ]);
  });
});
