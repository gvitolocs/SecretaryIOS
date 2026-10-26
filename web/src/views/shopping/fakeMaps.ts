import { vi } from "vitest";

/** Fake `loadMaps` boundary for the DealsMap tests: plain classes standing in
 * for google.maps.{Map,LatLngBounds,Marker} and the marker library's
 * AdvancedMarkerElement. Records what the view built so tests can assert on
 * chips, positions and fitBounds without loading any real Maps JS. */
export const mockMaps = {
  made: {
    maps: [] as Array<{ el: HTMLElement; opts: Record<string, unknown> }>,
    advanced: [] as Array<{ map: unknown; position: { lat: number; lng: number }; title: string; content?: HTMLElement }>,
    classic: [] as Array<{ map: unknown; position: { lat: number; lng: number }; title: string; label?: string }>,
    fit: null as unknown,
    fitPoints: [] as Array<{ lat: number; lng: number }>,
  },

  FakeBounds: class {
    pts: Array<{ lat: number; lng: number }> = [];
    extend(p: { lat: number; lng: number }): void {
      this.pts.push(p);
      mockMaps.made.fitPoints.push(p);
    }
  },

  FakeMap: class {
    constructor(el: HTMLElement, opts: Record<string, unknown>) {
      mockMaps.made.maps.push({ el, opts });
    }

    fitBounds(bounds: unknown): void {
      mockMaps.made.fit = bounds;
    }
  },

  FakeAdvancedMarker: class {
    map: unknown;
    constructor(opts: { map: unknown; position: { lat: number; lng: number }; title: string; content?: HTMLElement }) {
      this.map = opts.map;
      mockMaps.made.advanced.push(opts);
    }
  },

  FakeMarker: class {
    map: unknown;
    constructor(opts: { map: unknown; position: { lat: number; lng: number }; title: string; label?: string }) {
      this.map = opts.map;
      mockMaps.made.classic.push(opts);
    }
  },

  loadMaps: vi.fn(async () => ({
    maps: { Map: mockMaps.FakeMap, LatLngBounds: mockMaps.FakeBounds, Marker: mockMaps.FakeMarker },
    marker: { AdvancedMarkerElement: mockMaps.FakeAdvancedMarker },
  })),

  reset(): void {
    mockMaps.loadMaps.mockClear();
    mockMaps.made.maps = [];
    mockMaps.made.advanced = [];
    mockMaps.made.classic = [];
    mockMaps.made.fit = null;
    mockMaps.made.fitPoints = [];
  },
};
