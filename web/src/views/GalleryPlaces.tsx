import { A, useNavigate } from "@solidjs/router";
import { createEffect, createMemo, createResource, createSignal, For, on, onCleanup, Show } from "solid-js";
import { listPlaces, listSources, tileUrl, type GalleryKind, type PlaceCluster } from "../api/gallery";
import { SourceBar } from "../components/gallery/SourceBar";
import { BackIcon } from "../components/Icons";
import { LucinaMap } from "../components/LucinaMap";
import { t } from "../i18n";
import { supportsJxl } from "../lib/jxl";
import type { MapPin } from "../lib/mapKit";
import { badgeCount, clusterHref, clusterName, sameQuery, serverZoom, viewportBBox, type Viewport } from "../lib/placesView";
import { readString } from "../lib/storage";
import "../styles/gallery.css";
import "../styles/gallery-browse.css";

// /gallery/places: the pictures that have a position, as thumbnails on the map. The server groups the ones that are
// close together at the zoom the map is at (GET /v1/media/items/places); the map asks again after every move. A
// cluster opens its pictures in the Gallery grid (/gallery?bbox=…). Without map tiles the same clusters are a list.

const ORIGIN_KEY = "secretary.gallery.origin";
const MOVE_DEBOUNCE_MS = 250;

export default function GalleryPlaces() {
  const navigate = useNavigate();
  const [origin, setOrigin] = createSignal(readString("session", ORIGIN_KEY) ?? "all");
  const [kind, setKind] = createSignal<GalleryKind | "all">("all");
  const [view, setView] = createSignal<Viewport | null>(null);
  const [mapOk, setMapOk] = createSignal<boolean | undefined>(undefined);
  const [clusters, setClusters] = createSignal<PlaceCluster[]>([]);
  // Nothing at all has a position (the world view came back empty): not just an empty patch of sea.
  const [nothingAnywhere, setNothingAnywhere] = createSignal(false);
  const [settled, setSettled] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  const [sources] = createResource(() => listSources().catch(() => null));
  const [jxl] = createResource(supportsJxl);

  // A source that no longer exists (stale session choice) falls back to everything.
  const effectiveOrigin = createMemo(() => {
    const data = sources();
    const o = origin();
    return data && o !== "all" && !data.sources.some((s) => s.id === o) ? "all" : o;
  });

  let timer: ReturnType<typeof setTimeout> | undefined;
  let lastAsked: Viewport | null = null;
  let ticket = 0;
  onCleanup(() => clearTimeout(timer));

  async function fetchClusters(v: Viewport | null) {
    const mine = ++ticket;
    try {
      const result = await listPlaces({
        origin: effectiveOrigin(),
        kind: kind(),
        bbox: v?.bbox,
        zoom: v ? serverZoom(v.zoom) : 3,
        limit: 300,
      });
      if (mine !== ticket) return;
      setClusters(result.clusters);
      if (!v) setNothingAnywhere(result.total === 0);
      setFailed(false);
    } catch {
      if (mine === ticket) setFailed(true);
    } finally {
      if (mine === ticket) setSettled(true);
    }
  }

  // The first look is the whole world; then the map reports where it is, and every move asks again.
  createEffect(
    on([effectiveOrigin, kind], () => {
      setSettled(false);
      lastAsked = view();
      void fetchClusters(view());
    }),
  );

  function moved(v: { bbox: [number, number, number, number]; zoom: number }) {
    const next: Viewport = { bbox: viewportBBox(...v.bbox), zoom: v.zoom };
    setView(next);
    clearTimeout(timer);
    timer = setTimeout(() => {
      if (sameQuery(lastAsked, next)) return;
      lastAsked = next;
      void fetchClusters(next);
    }, MOVE_DEBOUNCE_MS);
  }

  const byId = createMemo(() => new Map(clusters().map((c) => [c.id, c])));
  const pins = createMemo<MapPin[]>(() =>
    clusters().map((c) => ({
      id: c.id,
      lat: c.lat,
      lng: c.lon,
      title: t("browse.placeOpen", { count: c.count }),
      subtitle: clusterName(c) || undefined,
    })),
  );

  const open = (c: PlaceCluster) => navigate(clusterHref(c));

  function renderPin(pin: MapPin): HTMLElement {
    const c = byId().get(pin.id);
    const el = document.createElement("button");
    el.type = "button";
    el.className = "g-cluster";
    const src = c ? tileUrl(c.cover, jxl() ?? false) : null;
    if (src) {
      const img = document.createElement("img");
      img.src = src;
      img.alt = "";
      img.draggable = false;
      img.loading = "lazy";
      el.append(img);
    }
    if (c) {
      const badge = document.createElement("span");
      badge.className = "g-cluster__count";
      badge.textContent = badgeCount(c.count);
      el.append(badge);
    }
    return el;
  }

  const seg = (value: GalleryKind | "all", label: string) => (
    <button type="button" class="g-seg__btn" aria-pressed={kind() === value} onClick={() => setKind(value)}>
      {label}
    </button>
  );

  return (
    <section class="gallery g-places" aria-labelledby="places-title">
      <div class="g-top">
        <header class="g-bar">
          <A class="g-back" href="/gallery" aria-label={t("gallery.backToGallery")}>
            <BackIcon />
          </A>
          <h1 class="g-title" id="places-title">
            {t("browse.placesTitle")}
          </h1>
          <div class="g-seg g-seg--end" role="group" aria-label={t("gallery.typeLabel")}>
            {seg("all", t("gallery.typeAll"))}
            {seg("photo", t("gallery.typePhotos"))}
            {seg("video", t("gallery.typeVideos"))}
          </div>
        </header>
        <Show when={(sources()?.sources.length ?? 0) > 0}>
          <SourceBar sources={sources()!.sources} all={sources()!.all} value={effectiveOrigin()} onChange={setOrigin} />
        </Show>
        <Show when={mapOk() === false}>
          <p class="g-note">{t("browse.placesNoMap")}</p>
        </Show>
      </div>
      <div class="g-main g-places__main">
        <Show when={mapOk() !== false}>
          <LucinaMap
            class="g-places__map"
            label={t("browse.placesMap")}
            pins={pins()}
            renderPin={renderPin}
            fit="first"
            onViewport={moved}
            onAvailable={setMapOk}
            onSelect={(id) => {
              const c = byId().get(id);
              if (c) open(c);
            }}
          />
        </Show>
        <Show when={mapOk() === false}>
          <ul class="g-places__list">
            <For each={clusters()}>
              {(c) => (
                <li>
                  <A class="g-place-row" href={clusterHref(c)}>
                    <Show when={tileUrl(c.cover, jxl() ?? false)}>{(src) => <img src={src()} alt="" loading="lazy" draggable={false} />}</Show>
                    <span>
                      <strong>{clusterName(c) || t("browse.placesTitle")}</strong>
                      <span>{t("browse.placesCount", { count: c.count })}</span>
                    </span>
                  </A>
                </li>
              )}
            </For>
          </ul>
        </Show>
        <Show when={failed()}>
          <p class="g-places__state" role="alert">
            {t("browse.placesLoadError")}
          </p>
        </Show>
        <Show when={settled() && !failed() && nothingAnywhere()}>
          <p class="g-places__state">{t("browse.placesEmpty")}</p>
        </Show>
      </div>
    </section>
  );
}
