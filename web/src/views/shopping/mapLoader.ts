// Loader for the Google Maps JavaScript API (deals map). Inserted as a
// script tag with `loading=async`, exactly once per page; `importLibrary`
// then gives us the `maps` and `marker` bindings without pulling @types.
// Declared as an npm dependency just for these few types is not worth it.

import type { MapStore } from "../../lib/dealsMap";

export interface MapsLatLng {
  lat: number;
  lng: number;
}

interface LatLngBounds {
  extend(p: MapsLatLng): void;
}

export interface MapsMap {
  fitBounds(b: LatLngBounds): void;
  setCenter(p: MapsLatLng): void;
}

export interface MapsApi {
  Map: new (el: HTMLElement, opts: { center: MapsLatLng; zoom: number; mapId?: string; disableDefaultUI?: boolean }) => MapsMap;
  LatLngBounds: new () => LatLngBounds;
  Marker: new (opts: {
    map: MapsMap | null;
    position: MapsLatLng;
    title: string;
    label?: string;
  }) => { map: MapsMap | null };
}

export interface MarkerApi {
  AdvancedMarkerElement: new (opts: {
    map: MapsMap | null;
    position: MapsLatLng;
    title: string;
    content?: HTMLElement;
  }) => { map: MapsMap | null };
}

export interface MapsLibraries {
  maps: MapsApi;
  /** Null when the browser or the API lacks the marker library. */
  marker: MarkerApi | null;
}

export interface MapsLoadOptions {
  key: string;
  mapId: string | null;
}

const API_SRC = "https://maps.googleapis.com/maps/api/js";
const SCRIPT_ID = "secretary-maps-js";

declare global {
  interface Window {
    google?: {
      maps?: { importLibrary?: (name: string) => Promise<unknown> };
    };
  }
}

let pending: Promise<MapsLibraries> | null = null;

/** Insert the Maps JS API once per page and resolve the libraries we use. */
export async function loadMaps(opts: MapsLoadOptions): Promise<MapsLibraries> {
  if (pending) return pending;
  pending = new Promise((resolve, reject) => {
    const script = document.createElement("script");
    script.id = SCRIPT_ID;
    script.async = true;
    script.src = `${API_SRC}?key=${encodeURIComponent(opts.key)}&loading=async&v=weekly&libraries=marker${
      opts.mapId ? `&mapId=${encodeURIComponent(opts.mapId)}` : ""
    }`;
    script.addEventListener("error", () => {
      pending = null;
      reject(new Error("map script failed to load"));
    });
    script.addEventListener("load", () => {
      void init();
    });
    document.head.append(script);

    async function init() {
      try {
        const importer = window.google?.maps?.importLibrary;
        if (!importer) throw new Error("Maps JS API did not initialize");
        const maps = await importer("maps");
        let marker: MarkerApi | null = null;
        try {
          marker = (await importer("marker")) as MarkerApi;
        } catch {
          // AdvancedMarkerElement unavailable: fall back to classic markers.
        }
        resolve({ maps: maps as MapsApi, marker });
      } catch (e) {
        pending = null;
        reject(e);
      }
    }
  });
  return pending;
}

/** Marker factory: an AdvancedMarkerElement with an HTML chip whenever the
 * marker library exists (a chip caption is optional), a classic labelled
 * Marker after `importLibrary("marker")` failed.
 * Returns the marker handle so the view can drop it by `map = null`. */
export function makeMarker(
  lib: MapsLibraries,
  opts: {
    map: MapsMap;
    store: MapStore;
    content: HTMLElement | undefined;
    initial: string;
  },
): { map: MapsMap | null } {
  const position = { lat: opts.store.lat, lng: opts.store.lng };
  if (lib.marker) {
    return new lib.marker.AdvancedMarkerElement({
      map: opts.map,
      position,
      title: opts.store.name,
      content: opts.content,
    });
  }
  return new lib.maps.Marker({
    map: opts.map,
    position,
    title: opts.store.name,
    label: opts.initial,
  });
}

/** `https://www.google.com/maps/dir/?api=1&destination=<lat>,<lng>` (opens
 * Google Maps with no extra API). */
export function directionsUrl(store: Pick<MapStore, "lat" | "lng">): string {
  return `https://www.google.com/maps/dir/?api=1&destination=${store.lat},${store.lng}`;
}
