import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { resetMapsConfig, resetRoutesCache } from "../api/maps";
import type { LucinaMapProps } from "../components/LucinaMap";
import { setLocale } from "../i18n";
import { localTime } from "../lib/mapKit";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";

const map = vi.hoisted(() => ({ props: null as LucinaMapProps | null }));
vi.mock("../components/LucinaMap", () => ({ LucinaMap: (props: LucinaMapProps) => { map.props = props; return <div data-testid="map" />; } }));
import PlaceMap from "./PlaceMap";

const home = { lat: 56.19633, lng: 10.2011, label: "Home" };
const polyline: [number, number][] = [[56.19633, 10.2011], [56.1611, 10.2006]];
const leaveAt = "2099-10-27T16:46:00Z";
const routes = ["transit", "walk", "bike", "drive"].map((mode, i) => ({ mode, durationS: 600 + i * 60, staticDurationS: 500, distanceM: 4200, polyline, leaveAt, transit: mode === "transit" ? [{ line: "2A", vehicle: "BUS", from: "Skejby", to: "Mølleparken", departAt: "2099-10-27T16:48:00Z", arriveAt: "2099-10-27T17:00:00Z" }] : [] }));
function serve(fail = false) {
  return stubFetch(call => {
    if (call.url === "/v1/maps/config") return { body: { tiles: null, home, assets: "/v1/maps/assets" } };
    if (call.url.startsWith("/v1/maps/routes")) return fail ? { status: 503 } : { body: { routes } };
    if (call.url.startsWith("/v1/maps/geocode")) return { body: { results: [{ name: "Event", address: "Aarhus", lat: 56.1611, lng: 10.2006 }] } };
    return { status: 404 };
  });
}
function show(query = "lat=56.1611&lng=10.2006&title=Event&when=2099-10-27T17%3A00%3A00Z") {
  return renderAt(`/map/place?${query}`, () => <Route path="/map/place" component={PlaceMap} />);
}
beforeEach(() => { setLocale("en"); resetMapsConfig(); resetRoutesCache(); localStorage.clear(); map.props = null; });

describe("place routes", () => {
  it("defaults to transit over 2 km, shows legs and leave-at, switches mode and remembers it", async () => {
    const calls = serve(); show();
    const transit = await screen.findByRole("button", { name: /Transit/ });
    expect(transit).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByText("Skejby → Mølleparken")).toBeInTheDocument();
    expect(screen.getByText(`Leave at ${localTime(leaveAt, "en")} to be on time`)).toBeInTheDocument();
    expect(map.props?.lines).toEqual([polyline]);
    fireEvent.click(screen.getByRole("button", { name: /Drive/ }));
    expect(screen.getByRole("button", { name: /Drive/ })).toHaveAttribute("aria-pressed", "true");
    expect(localStorage.getItem("lucina.map.mode")).toBe("drive");
    expect(screen.queryByText("Skejby → Mølleparken")).toBeNull();
    expect(screen.getByText("with traffic")).toBeInTheDocument();
    const request = calls.find(c => c.url.startsWith("/v1/maps/routes"))!;
    expect(new URL(request.url, "https://test.local").searchParams.get("arrive")).toBe("2099-10-27T17:00:00Z");
  });

  it("defaults to walking nearby and respects a remembered mode", async () => {
    serve(); show("lat=56.195&lng=10.201&title=Nearby");
    expect(await screen.findByRole("button", { name: /Walk/ })).toHaveAttribute("aria-pressed", "true");
  });

  it("uses a remembered mode even for a far place", async () => {
    localStorage.setItem("lucina.map.mode", "bike"); serve(); show();
    expect(await screen.findByRole("button", { name: /Bike/ })).toHaveAttribute("aria-pressed", "true");
  });

  it("falls back to labelled estimates with no route line on 503", async () => {
    const calls = serve(true); show();
    await waitFor(() => expect(calls.some(c => c.url.startsWith("/v1/maps/routes"))).toBe(true));
    expect(await screen.findByText("Travel times are estimates")).toBeInTheDocument();
    expect(screen.getByText(/≈ .*on foot/)).toBeInTheDocument();
    expect(map.props?.lines).toEqual([]);
    expect(screen.queryByRole("group", { name: "Travel mode" })).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("geocodes address-only places, then reroutes from the browser position", async () => {
    const calls = serve(); show("title=Event&address=Folkehuset");
    await screen.findByRole("button", { name: /Transit/ });
    expect(calls.some(c => c.url.startsWith("/v1/maps/geocode"))).toBe(true);
    map.props!.onUserLocation!({ lat: 56.16, lng: 10.20 });
    await waitFor(() => expect(calls.filter(c => c.url.startsWith("/v1/maps/routes"))).toHaveLength(2));
    expect(screen.getByText(/from you/)).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: /Walk/ })).toHaveAttribute("aria-pressed", "true");
  });
});
