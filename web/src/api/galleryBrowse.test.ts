import { afterEach, describe, expect, it, vi } from "vitest";
import { type Call, stubFetch } from "../test/fetch";
import { batch, galleryQueryString, listMonths, listOnThisDay, listPlaces, listRecent } from "./gallery";

afterEach(() => vi.unstubAllGlobals());

const rawItem = (id: string, extra: Record<string, unknown> = {}) => ({
  id,
  kind: "photo",
  title: `${id}.jpg`,
  mime: "image/jpeg",
  size: 1,
  taken_at: "2024-10-28T10:00:00Z",
  created_at: "2026-10-01T10:00:00Z",
  urls: { thumb: `/v1/media/items/${id}/thumb?size=256` },
  ...extra,
});

describe("the grid query", () => {
  it("carries the archive and a map window", () => {
    const q = new URLSearchParams(galleryQueryString({ archived: true, bbox: "12,55,13,56" }));
    expect(q.get("archived")).toBe("true");
    expect(q.get("bbox")).toBe("12,55,13,56");
    const plain = new URLSearchParams(galleryQueryString({}));
    expect(plain.has("archived")).toBe(false);
    expect(plain.has("bbox")).toBe(false);
  });

  it("asks the month counts of the archive too", async () => {
    const calls = stubFetch(() => ({ body: { months: [{ month: "2026-10", count: 2 }] } }));
    expect(await listMonths({ archived: true })).toEqual([{ month: "2026-10", count: 2 }]);
    expect(new URL(calls[0]!.url, "http://x").searchParams.get("archived")).toBe("true");
  });
});

describe("listPlaces", () => {
  it("sends the window, the zoom and the filters, and parses the clusters", async () => {
    let seen: Call | undefined;
    stubFetch((call) => {
      seen = call;
      return {
        body: {
          zoom: 6,
          total: 9,
          clusters: [
            { id: "1:2", count: 7, lat: 55.7, lon: 12.5, bbox: [12.4, 55.6, 12.6, 55.8], cover: rawItem("c1", { place: { name: "Town", lat: 55.7, lon: 12.5 } }) },
            // Malformed ones are dropped: no cover, no box, a non-numeric centre.
            { id: "bad1", count: 1, lat: 1, lon: 2, bbox: [1, 2, 3, 4] },
            { id: "bad2", count: 1, lat: 1, lon: 2, bbox: [1, 2, 3], cover: rawItem("x") },
            { id: "bad3", count: 1, lat: "1", lon: 2, bbox: [1, 2, 3, 4], cover: rawItem("y") },
          ],
        },
      };
    });
    const r = await listPlaces({ origin: "device:iphone-1", kind: "photo", bbox: [10, 50, 14, 58], zoom: 6, limit: 100 });
    const q = new URL(seen!.url, "http://x");
    expect(q.pathname).toBe("/v1/media/items/places");
    expect(Object.fromEntries(q.searchParams)).toEqual({ origin: "device:iphone-1", kind: "photo", bbox: "10,50,14,58", zoom: "6", limit: "100" });
    expect(r.total).toBe(9);
    expect(r.clusters).toHaveLength(1);
    expect(r.clusters[0]).toMatchObject({ id: "1:2", count: 7, lat: 55.7, lon: 12.5, bbox: [12.4, 55.6, 12.6, 55.8] });
    expect(r.clusters[0]!.cover.id).toBe("c1");
  });

  it("defaults to the whole Gallery, photos and videos", async () => {
    const calls = stubFetch(() => ({ body: { clusters: [] } }));
    const r = await listPlaces();
    const q = new URL(calls[0]!.url, "http://x").searchParams;
    expect([q.get("origin"), q.get("kind"), q.has("bbox"), q.has("zoom")]).toEqual(["all", "photo,video", false, false]);
    expect(r).toEqual({ zoom: 0, total: 0, clusters: [] });
  });
});

describe("listOnThisDay and listRecent", () => {
  it("parses the years and drops the empty ones", async () => {
    let seen: Call | undefined;
    stubFetch((call) => {
      seen = call;
      return {
        body: {
          years: [
            { year: 2023, years_ago: 3, total: 12, items: [rawItem("a"), { nope: true }, rawItem("b")] },
            { year: 2022, years_ago: 4, total: 1, items: [] },
            { years_ago: 1, items: [rawItem("c")] },
          ],
        },
      };
    });
    const years = await listOnThisDay({ date: "10-28", origin: "chat", perYear: 4 });
    const q = new URL(seen!.url, "http://x");
    expect(q.pathname).toBe("/v1/media/items/on-this-day");
    expect(Object.fromEntries(q.searchParams)).toEqual({ origin: "chat", date: "10-28", per_year: "4" });
    expect(years).toHaveLength(1);
    expect(years[0]).toMatchObject({ year: 2023, yearsAgo: 3, total: 12 });
    expect(years[0]!.items.map((i) => i.id)).toEqual(["a", "b"]);
  });

  it("asks for the recent arrivals", async () => {
    const calls = stubFetch(() => ({ body: { total: 5, items: [rawItem("n1"), rawItem("n2")] } }));
    const r = await listRecent({ days: 14, limit: 10 });
    const q = new URL(calls[0]!.url, "http://x");
    expect(q.pathname).toBe("/v1/media/items/recent");
    expect(Object.fromEntries(q.searchParams)).toEqual({ origin: "all", days: "14", limit: "10" });
    expect(r.total).toBe(5);
    expect(r.items.map((i) => i.id)).toEqual(["n1", "n2"]);
  });
});

describe("archive batches", () => {
  it("sends archive and unarchive like any other op", async () => {
    const calls = stubFetch((call) => ({ body: { done: (call.body as { ids: string[] }).ids, failed: [] } }));
    expect(await batch("archive", ["a", "b"])).toEqual({ done: ["a", "b"], failed: [] });
    await batch("unarchive", ["a"]);
    expect(calls.map((c) => c.body)).toEqual([{ op: "archive", ids: ["a", "b"] }, { op: "unarchive", ids: ["a"] }]);
  });
});
