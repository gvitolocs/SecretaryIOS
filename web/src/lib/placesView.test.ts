import { describe, expect, it } from "vitest";
import type { GalleryItem } from "../api/gallery";
import { badgeCount, clusterHref, clusterName, clusterWindow, sameQuery, serverZoom, viewportBBox } from "./placesView";

const cover = (place: GalleryItem["place"]) => ({ cover: { place } as GalleryItem });

describe("viewportBBox", () => {
  it("passes an ordinary window through", () => {
    expect(viewportBBox(10, 40, 14, 44)).toEqual([10, 40, 14, 44]);
  });
  it("is the whole world when the map shows more than the world", () => {
    expect(viewportBBox(-300, -50, 300, 60)).toEqual([-180, -50, 180, 60]);
    expect(viewportBBox(-180, -50, 180, 60)).toEqual([-180, -50, 180, 60]);
  });
  it("wraps a window across the date line to west > east", () => {
    expect(viewportBBox(170, -10, 190, 10)).toEqual([170, -10, -170, 10]);
    expect(viewportBBox(-190, -10, -170, 10)).toEqual([170, -10, -170, 10]);
  });
  it("keeps a window that ends on the date line ending there", () => {
    expect(viewportBBox(150, 0, 180, 10)).toEqual([150, 0, 180, 10]);
  });
  it("keeps the latitudes inside the Mercator range and ordered", () => {
    expect(viewportBBox(0, -89, 10, 89)).toEqual([0, -85.0511, 10, 85.0511]);
    expect(viewportBBox(0, 20, 10, -20)).toEqual([0, -20, 10, 20]);
  });
});

describe("serverZoom", () => {
  it("is one above MapLibre's zoom and stays in the server's range", () => {
    expect(serverZoom(5)).toBe(6);
    expect(serverZoom(-3)).toBe(0);
    expect(serverZoom(30)).toBe(24);
  });
});

describe("sameQuery", () => {
  const v = (zoom: number, bbox: [number, number, number, number]) => ({ zoom, bbox });
  it("ignores a nudge and notices a pan or a zoom", () => {
    const a = v(5, [10, 40, 20, 50]);
    expect(sameQuery(a, v(5.05, [10.05, 40.05, 20.05, 50.05]))).toBe(true);
    expect(sameQuery(a, v(5, [12, 40, 22, 50]))).toBe(false);
    expect(sameQuery(a, v(6, [10, 40, 20, 50]))).toBe(false);
    expect(sameQuery(null, null)).toBe(true);
    expect(sameQuery(a, null)).toBe(false);
  });
});

describe("cluster windows", () => {
  it("widen a point a hair so the picture itself still matches", () => {
    expect(clusterWindow({ bbox: [12.5683, 55.6761, 12.5683, 55.6761] })).toBe("12.56828,55.67608,12.56832,55.67612");
  });
  it("stay on the globe", () => {
    expect(clusterWindow({ bbox: [-180, -90, 180, 90] })).toBe("-180,-90,180,90");
  });
  it("link to the Gallery with the window in the query", () => {
    expect(clusterHref({ bbox: [1, 2, 3, 4] })).toBe("/gallery?bbox=0.99998%2C1.99998%2C3.00002%2C4.00002");
  });
});

describe("clusterName", () => {
  it("names a place with its region, or by itself, or not at all", () => {
    expect(clusterName(cover({ name: "Rome", admin1: "Lazio", country: "Italy", lat: 1, lon: 2 }))).toBe("Rome, Lazio");
    expect(clusterName(cover({ name: "Rome", admin1: null, country: null, lat: 1, lon: 2 }))).toBe("Rome");
    expect(clusterName(cover({ name: "Singapore", admin1: "Singapore", country: null, lat: 1, lon: 2 }))).toBe("Singapore");
    expect(clusterName(cover(null))).toBe("");
  });
});

describe("badgeCount", () => {
  it("shortens big counts", () => {
    expect([0, 7, 999, 1000, 1234, 9999, 10_000, 34_567].map(badgeCount)).toEqual(["0", "7", "999", "1k", "1.2k", "9.9k", "10k", "34k"]);
  });
});
