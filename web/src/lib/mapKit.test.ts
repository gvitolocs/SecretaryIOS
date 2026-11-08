import { describe, expect, it } from "vitest";
import {
  absoluteUrl,
  appleMapsUrl,
  distanceM,
  fitPadding,
  fitPlan,
  formatDistance,
  formatMinutes,
  googleDirectionsUrl,
  googleSearchUrl,
  NEAR_M,
  PIN_ZOOM,
  pinSetKey,
  placeFitPoints,
  placeFromMapsLink,
  placeFromQuery,
  placeRoute,
  placeTimeLabel,
  travelEstimates,
  validLatLng,
} from "./mapKit";

const home = { lat: 56.19633, lng: 10.2011 };
const folkehuset = { lat: 56.1611, lng: 10.2006 };

describe("distance", () => {
  it("measures great-circle metres", () => {
    expect(distanceM(home, home)).toBe(0);
    // home -> Folkehuset Møllestien: ~3.9 km straight
    expect(distanceM(home, folkehuset)).toBeGreaterThan(3850);
    expect(distanceM(home, folkehuset)).toBeLessThan(3950);
    // one degree of latitude ≈ 111.2 km
    expect(distanceM({ lat: 0, lng: 10 }, { lat: 1, lng: 10 })).toBeCloseTo(111_195, -2);
    expect(distanceM(home, folkehuset)).toBeCloseTo(distanceM(folkehuset, home), 6);
  });

  it("formats metres and kilometres per locale", () => {
    expect(formatDistance(237, "en")).toBe("240 m");
    expect(formatDistance(3917, "en")).toBe("3.9 km");
    expect(formatDistance(3917, "it")).toBe("3,9 km");
    expect(formatDistance(37_400, "da")).toBe("37 km");
  });
});

describe("travel estimates", () => {
  it("uses 4.8 / 15 / 35 km/h with a 1.3 detour factor", () => {
    // 10 km straight -> 13 km travelled
    expect(travelEstimates(10_000)).toEqual({ walk: 163, bike: 52, car: 22 });
    expect(travelEstimates(3917)).toEqual({ walk: 64, bike: 20, car: 9 });
    // never "0 min"
    expect(travelEstimates(5)).toEqual({ walk: 1, bike: 1, car: 1 });
  });

  it("formats minutes and hours", () => {
    expect(formatMinutes(9)).toBe("9 min");
    expect(formatMinutes(60)).toBe("1 h");
    expect(formatMinutes(163)).toBe("2 h 43 min");
  });
});

describe("external URLs", () => {
  it("builds the contract's directions and search URLs", () => {
    expect(appleMapsUrl({ ...folkehuset, title: "Hack Night: Rust & AI" })).toBe(
      "https://maps.apple.com/?daddr=56.1611,10.2006&q=Hack%20Night%3A%20Rust%20%26%20AI",
    );
    expect(appleMapsUrl(folkehuset)).toBe("https://maps.apple.com/?daddr=56.1611,10.2006");
    expect(googleDirectionsUrl(folkehuset)).toBe("https://www.google.com/maps/dir/?api=1&destination=56.1611,10.2006");
    expect(googleSearchUrl("Folkehuset Møllestien, Aarhus")).toBe(
      "https://www.google.com/maps/search/?api=1&query=Folkehuset%20M%C3%B8llestien%2C%20Aarhus",
    );
  });

  it("resolves server paths against the page", () => {
    expect(absoluteUrl("/v1/maps/tiles/region.pmtiles", "https://secratary.app/shopping/map")).toBe(
      "https://secratary.app/v1/maps/tiles/region.pmtiles",
    );
  });
});

describe("fitting", () => {
  it("pads by 12% of the side, clamped to 16..64, with room for pin and label", () => {
    expect(fitPadding(400, 300)).toEqual({ top: 60, right: 48, bottom: 52, left: 48 });
    expect(fitPadding(100, 100)).toEqual({ top: 40, right: 16, bottom: 32, left: 16 });
    expect(fitPadding(2000, 1000)).toEqual({ top: 88, right: 64, bottom: 80, left: 64 });
  });

  it("frames nothing, one pin at zoom 15, or the bounds of several", () => {
    expect(fitPlan([])).toEqual({ kind: "none" });
    expect(fitPlan([{ lat: 0, lng: 0 }])).toEqual({ kind: "none" });
    expect(fitPlan([home])).toEqual({ kind: "center", center: home, zoom: PIN_ZOOM });
    expect(fitPlan([home, { ...home }])).toEqual({ kind: "center", center: home, zoom: PIN_ZOOM });
    expect(fitPlan([home, folkehuset, { lat: 56.18, lng: 10.25 }])).toEqual({
      kind: "bounds",
      sw: { lat: 56.1611, lng: 10.2006 },
      ne: { lat: 56.19633, lng: 10.25 },
    });
  });

  it("frames the user with the place only when near", () => {
    expect(placeFitPoints(folkehuset, home)).toEqual([folkehuset, home]);
    expect(placeFitPoints(folkehuset, null)).toEqual([folkehuset]);
    const copenhagen = { lat: 55.6761, lng: 12.5683 };
    expect(distanceM(folkehuset, copenhagen)).toBeGreaterThan(NEAR_M);
    expect(placeFitPoints(folkehuset, copenhagen)).toEqual([folkehuset]);
  });

  it("keys a pin set by ids only", () => {
    expect(pinSetKey([{ id: "b" }, { id: "a" }])).toBe(pinSetKey([{ id: "a" }, { id: "b" }]));
    expect(pinSetKey([{ id: "a" }])).not.toBe(pinSetKey([{ id: "a" }, { id: "b" }]));
  });
});

describe("coordinates", () => {
  it("accepts finite in-range pairs except (0,0)", () => {
    expect(validLatLng(56.1, 10.2)).toBe(true);
    expect(validLatLng(-90, 180)).toBe(true);
    expect(validLatLng(0, 0)).toBe(false);
    expect(validLatLng(91, 0)).toBe(false);
    expect(validLatLng(0, -181)).toBe(false);
    expect(validLatLng(Number.NaN, 1)).toBe(false);
    expect(validLatLng("56", 10)).toBe(false);
    expect(validLatLng(56, undefined)).toBe(false);
  });
});

describe("shared map links", () => {
  it("reads Apple Maps, Google Maps and geo: links", () => {
    expect(placeFromMapsLink("https://maps.apple.com/?ll=56.1611,10.2006&q=Folkehuset")).toEqual({
      title: "Folkehuset",
      lat: 56.1611,
      lng: 10.2006,
    });
    expect(placeFromMapsLink("https://maps.apple.com/?address=Skejbyg%C3%A5rdsvej%203,%208240%20Risskov")).toEqual({
      title: "Skejbygårdsvej 3, 8240 Risskov",
      address: "Skejbygårdsvej 3, 8240 Risskov",
    });
    expect(placeFromMapsLink("https://www.google.com/maps/place/Folkehuset+M%C3%B8llestien/@56.1611,10.2006,17z")).toEqual({
      title: "Folkehuset Møllestien",
      lat: 56.1611,
      lng: 10.2006,
    });
    expect(placeFromMapsLink("https://www.google.com/maps/search/?api=1&query=56.1611%2C10.2006")).toEqual({
      title: "56.16110, 10.20060",
      lat: 56.1611,
      lng: 10.2006,
    });
    expect(placeFromMapsLink("https://www.google.com/maps/search/?api=1&query=Folkehuset%20Aarhus")).toEqual({
      title: "Folkehuset Aarhus",
      address: "Folkehuset Aarhus",
    });
    expect(placeFromMapsLink("geo:56.1611,10.2006?q=56.1611,10.2006(Folkehuset)")).toEqual({
      title: "Folkehuset",
      lat: 56.1611,
      lng: 10.2006,
    });
    expect(placeFromMapsLink("geo:0,0?q=Skejbyg%C3%A5rdsvej+3")).toEqual({
      title: "Skejbygårdsvej 3",
      address: "Skejbygårdsvej 3",
    });
  });

  it("ignores other links and unresolvable short links", () => {
    expect(placeFromMapsLink("https://example.com/?ll=1,2")).toBeNull();
    expect(placeFromMapsLink("https://maps.app.goo.gl/abc123")).toBeNull();
    expect(placeFromMapsLink("https://www.google.com/search?q=pizza")).toBeNull();
    expect(placeFromMapsLink("not a url")).toBeNull();
  });
});

describe("place route", () => {
  it("round-trips through the query", () => {
    const route = placeRoute({
      title: "Hack Night",
      address: "Folkehuset Møllestien, Aarhus",
      ...folkehuset,
      when: "2026-10-27T17:00:00.000Z",
      end: "2026-10-27T20:00:00.000Z",
    });
    expect(route.startsWith("/map/place?lat=56.1611&lng=10.2006&title=Hack+Night")).toBe(true);
    expect(placeFromQuery(route.split("?")[1]!)).toEqual({
      title: "Hack Night",
      address: "Folkehuset Møllestien, Aarhus",
      ...folkehuset,
      when: "2026-10-27T17:00:00.000Z",
      end: "2026-10-27T20:00:00.000Z",
      kind: "place",
    });
  });

  it("allows address only and drops bad coordinates and times", () => {
    expect(placeFromQuery("address=Skejbyg%C3%A5rdsvej+3")).toEqual({
      title: "Skejbygårdsvej 3",
      address: "Skejbygårdsvej 3",
      kind: "place",
    });
    expect(placeFromQuery("title=X&lat=0&lng=0&when=tomorrow")).toEqual({ title: "X", kind: "place" });
    expect(placeFromQuery("lat=56&lng=10")).toBeNull();
  });
});

describe("time label", () => {
  it("shows the start and the end, collapsing the same day", () => {
    const same = placeTimeLabel("2026-10-27T17:00:00", "2026-10-27T20:00:00", "en-GB");
    expect(same).toMatch(/27 Oct, 17:00–20:00$/);
    const multi = placeTimeLabel("2026-10-27T17:00:00", "2026-10-28T09:00:00", "en-GB");
    expect(multi).toMatch(/27 Oct, 17:00 – .*28 Oct, 09:00$/);
    expect(placeTimeLabel("2026-10-27T17:00:00", undefined, "en-GB")).toMatch(/27 Oct, 17:00$/);
    expect(placeTimeLabel("garbage", undefined, "en-GB")).toBe("");
  });
});

describe("route mode and local appointment times", () => {
  it("defaults to walking through 2 km, then transit, with a valid saved preference taking priority", async () => {
    const { defaultRouteMode } = await import("./mapKit");
    expect(defaultRouteMode(2000)).toBe("walk");
    expect(defaultRouteMode(2001)).toBe("transit");
    expect(defaultRouteMode(5000, "bike")).toBe("bike");
    expect(defaultRouteMode(5000, "invalid")).toBe("transit");
  });
  it("formats leave-at in the browser time zone and app locale", async () => {
    const { localTime, isRfc3339 } = await import("./mapKit");
    const date = "2099-10-27T16:46:00Z";
    for (const lang of ["en", "it", "da"]) {
      expect(localTime(date, lang)).toBe(new Intl.DateTimeFormat(lang, { hour: "2-digit", minute: "2-digit" }).format(new Date(date)));
    }
    expect(isRfc3339("2026-10-27 17:00Z")).toBe(false);
  });
});
