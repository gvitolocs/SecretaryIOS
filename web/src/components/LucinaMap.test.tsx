import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { resetMapsConfig } from "../api/maps";
import { setThemeSetting } from "../state/appearance";
import { stubFetch } from "../test/fetch";

const mock = vi.hoisted(() => ({ maps: [] as any[], markers: [] as any[], addProtocol: vi.fn() }));
vi.mock("maplibre-gl", () => ({
  addProtocol: mock.addProtocol, setWorkerUrl: vi.fn(),
  Map: class {
    options: any; events: Record<string, () => void> = {}; source: any; layer: any;
    touchZoomRotate = { disableRotation: vi.fn() }; keyboard = { disableRotation: vi.fn() };
    addControl = vi.fn(); remove = vi.fn(); jumpTo = vi.fn(); fitBounds = vi.fn(); flyTo = vi.fn();
    setStyle = vi.fn(() => { this.source = undefined; this.layer = undefined; this.events["style.load"]?.(); });
    constructor(options: any) { this.options = options; mock.maps.push(this); }
    on(event: string, callback: () => void) { this.events[event] = callback; }
    getZoom() { return 12; }
    isStyleLoaded() { return true; }
    getSource() { return this.source; }
    addSource(_id: string, source: any) { this.source = { ...source, setData: vi.fn((data: any) => { this.source.data = data; }) }; }
    getLayer() { return this.layer; }
    addLayer(layer: any) { this.layer = layer; }
  },
  Marker: class {
    element: HTMLElement; remove = vi.fn();
    constructor(options: { element: HTMLElement }) { this.element = options.element; mock.markers.push(this); }
    setLngLat(_point: number[]) { return this; }
    addTo(map: any) { map.options.container.append(this.element); return this; }
    getElement() { return this.element; }
  },
  AttributionControl: class {}, NavigationControl: class {},
}));
vi.mock("pmtiles", () => ({ Protocol: class { tile = vi.fn(); } }));
vi.mock("@protomaps/basemaps", () => ({ layers: () => [], namedFlavor: (name: string) => name }));
vi.mock("maplibre-gl/dist/maplibre-gl-worker.mjs?url", () => ({ default: "/worker.mjs" }));
import { LucinaMap } from "./LucinaMap";
const pin = { id: "place", title: "Folkehuset", label: "Folkehuset", lat: 56.1611, lng: 10.2006, selected: true };
beforeEach(() => { resetMapsConfig(); mock.maps.length = 0; mock.markers.length = 0; setThemeSetting("dark"); });
function serve(tiles: string | null = "/v1/maps/tiles/region.pmtiles") {
  return stubFetch(() => ({ body: { tiles, assets: "/v1/maps/assets", attribution: "Test attribution" } }));
}
describe("LucinaMap lifecycle", () => {
  it("uses same-origin assets, fits, selects and focuses a pin, and cleans up", async () => {
    serve(); const select = vi.fn(); const [focus, setFocus] = createSignal<{ id: string; tick: number }>();
    const view = render(() => <LucinaMap pins={[pin]} focus={focus()} onSelect={select} label="Map" />);
    await waitFor(() => expect(mock.maps).toHaveLength(1));
    const map = mock.maps[0];
    expect(map.options.style.sources.protomaps.url).toMatch(/^pmtiles:\/\/http:\/\/localhost.*\/v1\/maps\/tiles/);
    expect(map.options.style.glyphs).toContain("/v1/maps/assets/fonts/");
    expect(map.options.dragRotate).toBe(false);
    expect(map.jumpTo).toHaveBeenCalledWith(expect.objectContaining({ zoom: 15 }));
    fireEvent.click(screen.getByRole("button", { name: "Folkehuset" }));
    expect(select).toHaveBeenCalledWith("place");
    setFocus({ id: "place", tick: 1 });
    expect(map.flyTo).toHaveBeenCalledWith(expect.objectContaining({ center: [10.2006, 56.1611], zoom: 15 }));
    view.unmount(); expect(map.remove).toHaveBeenCalledOnce(); expect(mock.markers[0].remove).toHaveBeenCalled();
  });
  it("converts route coordinates, fits them, and restores the line after a theme change", async () => {
    serve(); const [lines, setLines] = createSignal<[number, number][][]>([[[56.19633, 10.2011], [pin.lat, pin.lng]]]);
    render(() => <LucinaMap pins={[pin]} lines={lines()} />);
    await waitFor(() => expect(mock.maps).toHaveLength(1));
    const map = mock.maps[0];
    expect(map.source.data.features[0].geometry.coordinates).toEqual([[10.2011, 56.19633], [10.2006, 56.1611]]);
    expect(map.fitBounds).toHaveBeenCalled();
    setThemeSetting("light");
    expect(map.setStyle).toHaveBeenCalledWith(expect.objectContaining({ sprite: expect.stringContaining("/sprites/v4/light") }));
    expect(map.source.data.features).toHaveLength(1);
    setLines([]); expect(map.source.data.features).toHaveLength(0);
  });
  it("watches location only when requested and clears the watch on unmount", async () => {
    serve(); let position!: PositionCallback;
    const watchPosition = vi.fn((callback: PositionCallback) => { position = callback; return 7; });
    const clearWatch = vi.fn();
    Object.defineProperty(navigator, "geolocation", { configurable: true, value: { watchPosition, clearWatch } });
    const location = vi.fn();
    const view = render(() => <LucinaMap pins={[pin]} showsUserLocation onUserLocation={location} />);
    await waitFor(() => expect(mock.maps).toHaveLength(1));
    position({ coords: { latitude: 56.2, longitude: 10.2 } } as GeolocationPosition);
    expect(location).toHaveBeenCalledWith({ lat: 56.2, lng: 10.2 });
    view.unmount(); expect(clearWatch).toHaveBeenCalledWith(7); watchPosition.mockClear();
    render(() => <LucinaMap pins={[pin]} interactive={false} />);
    await waitFor(() => expect(mock.maps).toHaveLength(2));
    expect(watchPosition).not.toHaveBeenCalled(); expect(mock.maps[1].options.interactive).toBe(false);
    delete (navigator as unknown as { geolocation?: unknown }).geolocation;
  });
  it("renders only the fallback without tiles", async () => {
    const calls = serve(null); const available = vi.fn();
    render(() => <LucinaMap pins={[pin]} fallback={<p>No map</p>} onAvailable={available} />);
    expect(await screen.findByText("No map")).toBeInTheDocument();
    expect(available).toHaveBeenCalledWith(false); expect(mock.maps).toHaveLength(0); expect(calls).toHaveLength(1);
  });
});
