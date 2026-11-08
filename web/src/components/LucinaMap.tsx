// The shared in-app map (maps contract, section 4): MapLibre GL over the
// self-hosted Protomaps tiles from `/v1/maps/*`, HTML pins styled with the
// --lu-* tokens, the owner's location dot, fit-to-pins and a focus request.
// MapLibre, pmtiles and the basemap style are loaded on first use, so the
// main bundle doesn't carry them; nothing is fetched from another origin.
// Without tiles (or without the map server) only `fallback` renders.

import { createEffect, createSignal, on, onCleanup, onMount, Show, type JSX } from "solid-js";
import type { GeoJSONSource, Map as MlMap, Marker as MlMarker } from "maplibre-gl";
import type { FeatureCollection, LineString } from "geojson";
import { mapsConfig, type MapsConfig } from "../api/maps";
import { locale } from "../i18n";
import {
  absoluteUrl,
  DEFAULT_HOME,
  fitPadding,
  fitPlan,
  MAX_FIT_ZOOM,
  PIN_ZOOM,
  pinSetKey,
  placeFitPoints,
  type LatLng,
  type MapPin,
} from "../lib/mapKit";
import { effectiveTheme } from "../state/appearance";
import "../styles/lucinaMap.css";

type MapLibre = typeof import("maplibre-gl");
type Basemaps = typeof import("@protomaps/basemaps");

let libs: Promise<{ ml: MapLibre; basemaps: Basemaps }> | null = null;

/** MapLibre (+ its CSS and worker), pmtiles and the basemap layers, once per page. */
function loadLibs(): Promise<{ ml: MapLibre; basemaps: Basemaps }> {
  libs ??= Promise.all([
    import("maplibre-gl"),
    import("pmtiles"),
    import("@protomaps/basemaps"),
    import("maplibre-gl/dist/maplibre-gl-worker.mjs?url"),
    import("maplibre-gl/dist/maplibre-gl.css"),
  ]).then(([ml, pmtiles, basemaps, worker]) => {
    // The bundled worker is a same-origin module file (CSP worker-src 'self').
    ml.setWorkerUrl(worker.default);
    const protocol = new pmtiles.Protocol();
    ml.addProtocol("pmtiles", protocol.tile as Parameters<MapLibre["addProtocol"]>[1]);
    return { ml, basemaps };
  });
  libs.catch(() => {
    libs = null;
  });
  return libs;
}

/** Whether this page can show maps at all (tiles installed on the server). */
export async function mapAvailable(): Promise<boolean> {
  return !!(await mapsConfig())?.tiles;
}

function buildStyle(basemaps: Basemaps, cfg: MapsConfig, flavor: "dark" | "light") {
  const assets = absoluteUrl(cfg.assets).replace(/\/$/, "");
  return {
    version: 8 as const,
    glyphs: `${assets}/fonts/{fontstack}/{range}.pbf`,
    sprite: `${assets}/sprites/v4/${flavor}`,
    sources: {
      protomaps: {
        type: "vector" as const,
        url: `pmtiles://${absoluteUrl(cfg.tiles!)}`,
        attribution: cfg.attribution,
      },
    },
    layers: basemaps.layers("protomaps", basemaps.namedFlavor(flavor), { lang: locale() }),
  };
}

/** The default pin: a tinted dot with the title as its label (and a caption under it). */
function defaultPin(pin: MapPin, interactive: boolean): HTMLElement {
  const el = document.createElement(interactive ? "button" : "span");
  if (el instanceof HTMLButtonElement) el.type = "button";
  el.className = "lmap-pin";
  const dot = document.createElement("span");
  dot.className = "lmap-pin-dot";
  dot.setAttribute("aria-hidden", "true");
  el.append(dot);
  const caption = pin.label ?? (pin.selected ? pin.title : undefined);
  if (caption) {
    const label = document.createElement("span");
    label.className = "lmap-pin-label";
    label.textContent = caption;
    label.setAttribute("aria-hidden", "true");
    el.append(label);
  }
  return el;
}

export interface LucinaMapProps {
  pins: MapPin[];
  /** Route points, latitude first as returned by the server. */
  lines?: [number, number][][];
  /** Space occupied by overlays, in addition to pin padding. */
  insets?: Partial<Record<"top" | "right" | "bottom" | "left", number>>;
  showsUserLocation?: boolean;
  onSelect?: (id: string) => void;
  /** Every new fix of the owner's position (only with showsUserLocation). */
  onUserLocation?: (p: LatLng) => void;
  /** Animate to this pin whenever `tick` changes. */
  focus?: { id: string; tick: number } | null;
  /** False: a static mini-map (no handlers, no controls, no pointer events). */
  interactive?: boolean;
  /** Frame the first pin with the owner's position when they're near (place screen). */
  fitWithUser?: boolean;
  /** Custom pin content (the deals chips); LucinaMap adds the label, tint and click. */
  renderPin?: (pin: MapPin) => HTMLElement;
  /** Shown instead of the map when no tiles are available. */
  fallback?: JSX.Element;
  onAvailable?: (available: boolean) => void;
  /** The map's window after every move (and once it has loaded): west, south, east, north and MapLibre's zoom. */
  onViewport?: (view: { bbox: [number, number, number, number]; zoom: number }) => void;
  /**
   * `pins` (default): frame the pins whenever the set of pins changes. `first`: only the first time there are pins; a
   * map whose pins follow the viewport (Places) must not fight the owner's panning.
   */
  fit?: "pins" | "first";
  class?: string;
  label?: string;
}

export function LucinaMap(props: LucinaMapProps) {
  const interactive = props.interactive !== false;
  const [cfg, setCfg] = createSignal<MapsConfig | null | undefined>(undefined);
  const [map, setMap] = createSignal<MlMap | null>(null);
  const [user, setUser] = createSignal<LatLng | null>(null);
  let host: HTMLDivElement | undefined;
  let alive = true;
  let watchId: number | null = null;
  let userMarker: MlMarker | null = null;
  let userFitted = false;
  const markers = new Map<string, MlMarker>();
  let ml: MapLibre | null = null;
  let basemaps: Basemaps | null = null;

  const available = () => !!cfg()?.tiles;

  onMount(() => {
    void mapsConfig().then((c) => {
      if (!alive) return;
      setCfg(c);
      props.onAvailable?.(!!c?.tiles);
      if (c?.tiles) void start(c).catch(() => {
        if (alive) { map()?.remove(); setMap(null); setCfg(null); props.onAvailable?.(false); }
      });
    });
    if (props.showsUserLocation && typeof navigator !== "undefined" && navigator.geolocation) {
      try {
        watchId = navigator.geolocation.watchPosition(
          (pos) => {
            const p = { lat: pos.coords.latitude, lng: pos.coords.longitude };
            setUser(p);
            props.onUserLocation?.(p);
          },
          () => undefined, // denied or unavailable: no dot, no message
          { enableHighAccuracy: false, maximumAge: 60_000, timeout: 30_000 },
        );
      } catch {
        watchId = null;
      }
    }
  });

  async function start(c: MapsConfig) {
    let loaded: { ml: MapLibre; basemaps: Basemaps };
    try {
      loaded = await loadLibs();
    } catch {
      if (alive) {
        setCfg(null);
        props.onAvailable?.(false);
      }
      return;
    }
    if (!alive || !host) return;
    ml = loaded.ml;
    basemaps = loaded.basemaps;
    const plan = fitPlan(props.pins);
    const home = c.home ?? DEFAULT_HOME;
    const m = new ml.Map({
      container: host,
      style: buildStyle(basemaps, c, effectiveTheme()),
      center: plan.kind === "center" ? [plan.center.lng, plan.center.lat] : [home.lng, home.lat],
      zoom: plan.kind === "center" ? plan.zoom : 12,
      interactive,
      attributionControl: false,
      dragRotate: false,
      pitchWithRotate: false,
      touchPitch: false,
      maxPitch: 0,
      fadeDuration: interactive ? 300 : 0,
    });
    m.touchZoomRotate.disableRotation();
    m.keyboard.disableRotation();
    m.addControl(new ml.AttributionControl({ compact: true }), "bottom-right");
    if (interactive) m.addControl(new ml.NavigationControl({ showCompass: false }), "top-right");
    m.on("style.load", () => drawLines(m));
    if (props.onViewport) {
      const emit = () => {
        const b = m.getBounds();
        props.onViewport?.({ bbox: [b.getWest(), b.getSouth(), b.getEast(), b.getNorth()], zoom: m.getZoom() });
      };
      m.on("moveend", emit);
      m.once("load", emit);
    }
    setMap(m);
  }

  function frame(points: LatLng[], animate: boolean) {
    const m = map();
    if (!m || !host) return;
    const plan = fitPlan(points);
    const base = fitPadding(host.clientWidth, host.clientHeight);
    const padding = { top: base.top + (props.insets?.top ?? 0), right: base.right + (props.insets?.right ?? 0), bottom: base.bottom + (props.insets?.bottom ?? 0), left: base.left + (props.insets?.left ?? 0) };
    if (plan.kind === "center") m.jumpTo({ center: [plan.center.lng, plan.center.lat], zoom: plan.zoom, padding });
    else if (plan.kind === "bounds") {
      m.fitBounds(
        [
          [plan.sw.lng, plan.sw.lat],
          [plan.ne.lng, plan.ne.lat],
        ],
        { padding, maxZoom: MAX_FIT_ZOOM, animate },
      );
    }
  }

  // Pins: rebuild a marker when its pin changed, drop the ones that are gone.
  createEffect(() => {
    const m = map();
    if (!m || !ml) return;
    const seen = new Set<string>();
    for (const pin of props.pins) {
      seen.add(pin.id);
      const old = markers.get(pin.id);
      if (old) old.remove();
      const el = props.renderPin ? props.renderPin(pin) : defaultPin(pin, interactive);
      el.classList.add("lmap-marker", `lmap-tint--${pin.tint ?? "accent"}`);
      el.classList.toggle("lmap-marker--selected", !!pin.selected);
      el.setAttribute("aria-label", pin.subtitle ? `${pin.title}, ${pin.subtitle}` : pin.title);
      el.title = pin.title;
      if (interactive) {
        el.addEventListener("click", (e) => {
          e.stopPropagation();
          props.onSelect?.(pin.id);
        });
      }
      const marker = new ml.Marker({ element: el, anchor: props.renderPin ? "center" : "top" })
        .setLngLat([pin.lng, pin.lat])
        .addTo(m);
      marker.getElement().style.zIndex = pin.selected ? "3" : pin.tint === "home" ? "1" : "2";
      markers.set(pin.id, marker);
    }
    for (const [id, marker] of markers) {
      if (!seen.has(id)) {
        marker.remove();
        markers.delete(id);
      }
    }
  });

  function drawLines(m: MlMap) {
    // style.load can fire while sprites and glyphs are still loading: draw once the map is idle.
    if (!m.isStyleLoaded()) {
      m.once("idle", () => drawLines(m));
      return;
    }
    const data: FeatureCollection<LineString> = {
      type: "FeatureCollection",
      features: (props.lines ?? []).filter(line => line.length > 1).map(line => ({
        type: "Feature", properties: {}, geometry: { type: "LineString", coordinates: line.map(([lat, lng]) => [lng, lat]) },
      })),
    };
    const source = m.getSource("lucina-routes");
    if (source && "setData" in source) (source as GeoJSONSource).setData(data);
    else m.addSource("lucina-routes", { type: "geojson", data });
    if (!m.getLayer("lucina-routes")) m.addLayer({ id: "lucina-routes", type: "line", source: "lucina-routes", layout: { "line-cap": "round", "line-join": "round" }, paint: { "line-color": getComputedStyle(host!).getPropertyValue("--lu-accent").trim() || "#9580ff", "line-width": 5, "line-opacity": 0.85 } });
  }

  createEffect(() => {
    const m = map();
    const lines = props.lines ?? [];
    if (!m) return;
    drawLines(m);
    if (lines.some(line => line.length > 1)) frame([...props.pins, ...lines.flat().map(([lat, lng]) => ({ lat, lng }))], true);
  });

  createEffect(on(() => props.insets, () => {
    if (!map()) return;
    const first = props.pins[0];
    const points = props.fitWithUser && first ? placeFitPoints(first, user()) : props.pins;
    frame([...points, ...(props.lines ?? []).flat().map(([lat, lng]) => ({ lat, lng }))], false);
  }));

  // Fit whenever the set of pin ids changes (not on selection or label changes).
  let framedOnce = false;
  createEffect(
    on([map, () => pinSetKey(props.pins)], ([m], prev) => {
      if (!m) return;
      if (props.fit === "first") {
        if (framedOnce || !props.pins.length) return;
        framedOnce = true;
      }
      frame([...props.pins, ...(props.lines ?? []).flat().map(([lat, lng]) => ({ lat, lng }))], prev !== undefined && prev[0] === m);
    }),
  );

  // The owner's dot, and the place screen framing them with the place once.
  createEffect(() => {
    const m = map();
    const p = user();
    if (!m || !ml || !p) return;
    if (!userMarker) {
      const el = document.createElement("span");
      el.className = "lmap-user";
      el.setAttribute("aria-hidden", "true");
      userMarker = new ml.Marker({ element: el }).setLngLat([p.lng, p.lat]).addTo(m);
    } else userMarker.setLngLat([p.lng, p.lat]);
    const first = props.pins[0];
    if (props.fitWithUser && !props.lines?.length && !userFitted && first) {
      userFitted = true;
      frame(placeFitPoints(first, p), true);
    }
  });

  createEffect(
    on(
      [map, () => props.focus?.tick],
      () => {
        const m = map();
        const f = props.focus;
        const pin = f ? props.pins.find((p) => p.id === f.id) : undefined;
        if (!m || !pin) return;
        m.flyTo({ center: [pin.lng, pin.lat], zoom: Math.max(m.getZoom(), PIN_ZOOM), essential: true });
      },
      { defer: true },
    ),
  );

  // Dark/light follows the app theme (and the system, through it), live.
  createEffect(
    on(
      effectiveTheme,
      (flavor) => {
        const m = map();
        const c = cfg();
        if (m && c?.tiles && basemaps) m.setStyle(buildStyle(basemaps, c, flavor));
      },
      { defer: true },
    ),
  );

  onCleanup(() => {
    alive = false;
    if (watchId !== null) navigator.geolocation?.clearWatch(watchId);
    for (const marker of markers.values()) marker.remove();
    markers.clear();
    userMarker?.remove();
    map()?.remove();
  });

  return (
    <Show when={cfg() === undefined || available()} fallback={props.fallback}>
      <div
        ref={host}
        class={`lmap ${props.class ?? ""}`}
        classList={{ "lmap--static": !interactive }}
        role={interactive ? "region" : "img"}
        aria-label={props.label}
      />
    </Show>
  );
}

export default LucinaMap;
