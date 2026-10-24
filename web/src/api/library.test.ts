import { describe, expect, it } from "vitest";
import { stubFetch } from "../test/fetch";
import { libraryApi } from "./library";

describe("library client", () => {
  it("sends list filters including optional kinds, and retains unknown progress", async () => {
    const calls = stubFetch(() => ({
      body: { books: [{ id: "bk_1", status: "unknown", progress: { percent: null, location: 12, page: null }, kind: "document" }] },
    }));
    const books = await libraryApi.list({ q: "room & board", status: "unknown", onKindle: false, kind: "book,document", limit: 20 });
    const url = new URL(calls[0]!.url, "http://x");
    expect(Object.fromEntries(url.searchParams)).toEqual({
      q: "room & board",
      status: "unknown",
      onKindle: "false",
      kind: "book,document",
      limit: "20",
    });
    expect(books[0]!.progress!.percent).toBeNull();
    expect(books[0]!.status).toBe("unknown");
    expect(books[0]!.kind).toBe("document");
  });
  it("keeps highlight dedup keys opaque and encodes the book path", async () => {
    const calls = stubFetch(() => ({ body: { highlights: [{ dedupKey: "totally-different|format:😀" }] } }));
    const highlights = await libraryApi.highlights("book/1", "writing", 25);
    expect(calls[0]!.url).toBe("/v1/library/books/book%2F1/highlights?q=writing&limit=25");
    expect(highlights[0]!.dedupKey).toBe("totally-different|format:😀");
  });
  it("uses every remaining Library route and sends the version on edits", async () => {
    const calls = stubFetch(() => ({ body: {} }));
    await libraryApi.get("bk_1");
    await libraryApi.update("bk_1", { status: "abandoned", tags: ["essay"] }, "version-1");
    await libraryApi.search("room");
    await libraryApi.recent(30);
    await libraryApi.stats();
    await libraryApi.sync();
    await libraryApi.import({ source: "kindle-usb", device: null, scannedAt: "2026-10-24T12:00:00Z", books: [], clippings: [] });
    await libraryApi.brief("bk_1");
    expect(calls.map((c) => [c.method, c.url])).toEqual([
      ["GET", "/v1/library/books/bk_1"],
      ["PATCH", "/v1/library/books/bk_1"],
      ["GET", "/v1/library/search?q=room&limit=200"],
      ["GET", "/v1/library/recent?days=30&limit=500"],
      ["GET", "/v1/library/stats"],
      ["GET", "/v1/library/sync"],
      ["POST", "/v1/library/import"],
      ["POST", "/v1/library/books/bk_1/brief"],
    ]);
    expect(calls[1]!.body).toEqual({ status: "abandoned", tags: ["essay"], baseUpdatedAt: "version-1" });
    for (const c of calls.filter((c) => c.method !== "GET")) {
      expect(c.headers["x-secretary-csrf"]).toBe("1");
      expect(c.headers["x-secretary-client"]).toBe("web");
      expect(c.credentials).toBe("same-origin");
    }
  });
});
