import { beforeEach, describe, expect, it } from "vitest";
import { stubFetch } from "../test/fetch";
import { mapRoutes, mapsConfig, parseMapsConfig, parseRoutes, resetMapsConfig, resetRoutesCache } from "./maps";

const from = { lat: 56.19633, lng: 10.2011 };
const to = { lat: 56.1611, lng: 10.2006 };
const route = { mode: "drive", durationS: 540, staticDurationS: 480, distanceM: 4200, polyline: [[56.19633, 10.2011], [56.1611, 10.2006]], leaveAt: "2099-10-27T16:46:00Z" };
beforeEach(() => { resetRoutesCache(); resetMapsConfig(); });

describe("maps API", () => {
  it("accepts valid routes and transit legs, rejects malformed routes and dates", () => {
    const routes = parseRoutes({ routes: [route, { ...route, mode: "transit", transit: [{ line: "2A", vehicle: "BUS", from: "Skejby", to: "Aarhus", departAt: "2099-10-27T16:46:00Z", arriveAt: "bad" }, null] }, { ...route, mode: "flying" }, { ...route, mode: "bike", durationS: -1 }, { ...route, mode: "walk", distanceM: Infinity }, route] });
    expect(routes.map(r => r.mode)).toEqual(["drive", "transit"]);
    expect(routes[0]).toMatchObject(route);
    expect(routes[1]?.transit).toEqual([{ line: "2A", vehicle: "BUS", from: "Skejby", to: "Aarhus", departAt: "2099-10-27T16:46:00Z", arriveAt: undefined }]);
    expect(parseRoutes(null)).toEqual([]);
    expect(parseRoutes({ routes: [{ ...route, polyline: [[NaN, 10]], leaveAt: "bad" }] })[0]).toMatchObject({ polyline: [], leaveAt: undefined });
  });

  it("caches concurrent requests, includes future arrival and omits past appointments", async () => {
    const calls = stubFetch(() => ({ body: { routes: [route] } }));
    const first = mapRoutes(from, to, "2099-10-27T17:00:00Z");
    expect(mapRoutes(from, to, "2099-10-27T17:00:00Z")).toBe(first);
    expect(await first).toHaveLength(1);
    const url = new URL(calls[0]!.url, "https://example.test");
    expect(url.searchParams.get("from")).toBe("56.1963,10.2011");
    expect(url.searchParams.get("to")).toBe("56.1611,10.2006");
    expect(url.searchParams.get("arrive")).toBe("2099-10-27T17:00:00Z");
    await mapRoutes(from, to, "2020-01-01T00:00:00Z");
    expect(calls[1]!.url).not.toContain("arrive");
  });

  it("returns an empty list on provider failure", async () => {
    stubFetch(() => ({ status: 503 }));
    expect(await mapRoutes(from, to)).toEqual([]);
  });

  it("fetches config once even when unavailable and rejects remote asset URLs", async () => {
    const calls = stubFetch(() => ({ status: 401 }));
    expect(await mapsConfig()).toBeNull();
    expect(await mapsConfig()).toBeNull();
    expect(calls).toHaveLength(1);
    expect(parseMapsConfig({ tiles: "https://elsewhere.test/tiles", assets: "//elsewhere.test/assets" })).toMatchObject({ tiles: null, assets: "/v1/maps/assets" });
  });
});

it("rejects backslash paths that URL parsers treat as another origin", () => {
  expect(parseMapsConfig({ tiles: "/\\other.test/archive", assets: "/\\other.test/assets" })).toMatchObject({ tiles: null, assets: "/v1/maps/assets" });
});
