// Deals map (/shopping/map): nearby supermarkets with Lucina's current
// offers. Google Maps only when the server has a web key — chip markers,
// home marker, fitted bounds; otherwise the store list is the whole view.

import { A } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { createStore } from "solid-js/store";
import { shoppingApi } from "../../api/shopping";
import { toApiError } from "../../api/client";
import {
  chainHue,
  chainInitial,
  chipCaption,
  distanceLabel,
  type Deal,
  type DealsMapData,
  type MapStore,
} from "../../lib/dealsMap";
import { shop, t } from "../../state/shopping";
import "../../styles/shopping.css";
import "../../styles/dealsMap.css";
import { directionsUrl, loadMaps, makeMarker, type MapsLibraries, type MapsMap } from "./mapLoader";

const FALLBACK_ZOOM = 13;

const BROWSER_TAG: Record<string, string> = { it: "it-IT", en: "en-GB", da: "da-DK" };

function num(n: number): string {
  return new Intl.NumberFormat(BROWSER_TAG[shop.lang] ?? "da-DK", { minimumFractionDigits: 2, maximumFractionDigits: 2 }).format(n);
}

function day(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return new Intl.DateTimeFormat(BROWSER_TAG[shop.lang] ?? "da-DK", { day: "numeric", month: "short" }).format(d);
}

/** Marker chip content (AdvancedMarkerElement): chain initial + offer badge. */
function chipElement(store: MapStore, onOpen: (id: string) => void): HTMLElement {
  const el = document.createElement("button");
  el.type = "button";
  el.className = "deals-chip";
  el.style.setProperty("--chip-h", String(chainHue(store)));
  el.title = store.name;
  const letter = document.createElement("span");
  letter.className = "deals-chip-letter";
  letter.textContent = chainInitial(store);
  el.append(letter);
  const caption = chipCaption(store);
  if (caption) {
    const badge = document.createElement("span");
    badge.className = "deals-chip-badge";
    badge.textContent = caption;
    el.append(badge);
  }
  el.addEventListener("click", (e) => {
    e.stopPropagation();
    onOpen(store.id);
  });
  return el;
}

export default function DealsMapView() {
  const [status, setStatus] = createSignal<"loading" | "ready" | "error">("loading");
  const [error, setError] = createSignal<string | null>(null);
  const [data, setData] = createSignal<DealsMapData | null>(null);
  const [filters, setFilters] = createStore({ myList: false, clearance: false, chain: "" });
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  const [mapsReady, setMapsReady] = createSignal(false);
  const [mapError, setMapError] = createSignal(false);
  let mapHost: HTMLDivElement | undefined;
  let lists: HTMLUListElement | undefined;
  // Non-reactive handles: the Maps API objects are never read by the renderer.
  let maps: MapsLibraries | null = null;
  let map: MapsMap | null = null;
  let fitted = false;
  let dropped: Array<{ map: MapsMap | null }> = [];

  const stores = createMemo(() => data()?.stores ?? []);
  const filtered = createMemo(() =>
    stores().filter(
      (s) =>
        (!filters.myList || s.listMatches.length > 0) &&
        (!filters.clearance || s.clearances.length > 0) &&
        (!filters.chain || s.chainKey === filters.chain),
    ),
  );
  const chains = createMemo(() => {
    const byKey = new Map<string, string>();
    for (const s of stores()) {
      const key = s.chainKey ?? s.id;
      if (!byKey.has(key)) byKey.set(key, s.chain ?? s.name);
    }
    return [...byKey].map(([key, label]) => ({ key, label }));
  });
  const selected = createMemo(() => stores().find((s) => s.id === selectedId()) ?? null);

  async function refresh() {
    setStatus("loading");
    setError(null);
    try {
      const d = await shoppingApi.dealsMap();
      setData(d);
      setStatus("ready");
      if (d.maps.webKey) {
        loadMaps({ key: d.maps.webKey, mapId: d.maps.mapId })
          .then((lib) => {
            maps = lib;
            setMapsReady(true);
          })
          .catch(() => setMapError(true));
      }
    } catch (e) {
      setError(toApiError(e).message);
      setStatus("error");
    }
  }

  onMount(() => void refresh());

  // A dropped marker stays only while the map exists: signals flip → redraw.
  createEffect(() => {
    if (!mapsReady()) return;
    const lib = maps;
    if (!lib || !mapHost) return;
    for (const m of dropped) m.map = null;
    dropped = [];
    if (!map) {
      const home = data()!.home;
      map = new lib.maps.Map(mapHost, {
        center: { lat: home.lat, lng: home.lng },
        zoom: FALLBACK_ZOOM,
        mapId: data()!.maps.mapId ?? undefined,
        disableDefaultUI: false,
      });
      // Home pin: chip content when the marker library is there, classic otherwise.
      const homePos = { lat: data()!.home.lat, lng: data()!.home.lng };
      const pin = document.createElement("div");
      pin.className = "deals-home";
      pin.title = t("mapTitle");
      pin.textContent = "🏠";
      dropped.push(
        lib.marker
          ? new lib.marker.AdvancedMarkerElement({ map, position: homePos, title: "Home", content: pin })
          : new lib.maps.Marker({ map, position: homePos, title: "Home" }), // classic fallback: no label
      );
    }
    if (!fitted && lib.maps.LatLngBounds) {
      const bounds = new lib.maps.LatLngBounds();
      bounds.extend({ lat: data()!.home.lat, lng: data()!.home.lng });
      for (const s of filtered()) bounds.extend({ lat: s.lat, lng: s.lng });
      map!.fitBounds(bounds);
      fitted = true;
    }
    for (const s of filtered()) {
      const withCaption = chipCaption(s) !== null;
      const mark = makeMarker(lib, {
        map: map!,
        store: s,
        initial: chainInitial(s),
        content: withCaption ? chipElement(s, (id) => openStore(id)) : undefined,
      });
      dropped.push(mark);
    }
  });

  onCleanup(() => {
    for (const m of dropped) m.map = null;
    dropped = [];
    map = null;
  });

  function openStore(id: string) {
    setSelectedId(id);
    // bring the row next to the panel into view (mobile scrolling)
    const row = lists?.querySelector(`[data-store="${CSS.escape(id)}"]`);
    row?.scrollIntoView?.({ block: "nearest" });
  }

  function toggle(key: "myList" | "clearance") {
    setFilters(key, !filters[key]);
  }

  const DealRow = (p: { deal: Deal }) => (
    <li class="deals-deal" classList={{ "deals-deal--flyer": p.deal.source === "flyer" }}>
      <span class="deals-deal-name">{p.deal.name}</span>
      <span class="deals-deal-price">
        {num(p.deal.priceDkk)} kr
        {p.deal.normalPriceDkk !== null && <s class="deals-deal-was"> {t("mapWas", { p: num(p.deal.normalPriceDkk) })} kr</s>}
        {p.deal.discountPct !== null && <b class="deals-deal-badge">−{p.deal.discountPct}%</b>}
      </span>
      <span class="deals-deal-meta">
        {p.deal.unitPrice && <span>{p.deal.unitPrice}</span>}
        {p.deal.stock !== null && <span>{t("mapStock", { n: p.deal.stock, unit: p.deal.stockUnit ?? "" })}</span>}
        <span>
          {t("mapValidUntil", { d: day(p.deal.validTo) })}
          {p.deal.status === "upcoming" && ` · ${t("mapUpcoming")}`}
        </span>
        {p.deal.source === "flyer" && <span class="deals-deal-src">{t("mapFlyer")}</span>}
      </span>
    </li>
  );

  return (
    <div class="bring deals" lang={shop.lang}>
      <header class="deals-head">
        <A class="bring-back-pill deals-back" href="/shopping" aria-label={t("mapBackList")}>
          <span class="bring-chevron-back" aria-hidden="true" />
          <span>{t("mapBackList")}</span>
        </A>
        <h1 class="deals-title">{t("mapTitle")}</h1>
      </header>

      <div class="deals-filters" role="group" aria-label={t("mapTitle")}>
        <button type="button" class="deals-chipbtn" aria-pressed={filters.myList} onClick={() => toggle("myList")}>
          {t("mapMyListChip")}
        </button>
        <button type="button" class="deals-chipbtn" aria-pressed={filters.clearance} onClick={() => toggle("clearance")}>
          {t("mapClearanceChip")}
        </button>
        <For each={chains()}>
          {(c) => (
            <button
              type="button"
              class="deals-chipbtn"
              aria-pressed={filters.chain === c.key}
              onClick={() => setFilters("chain", filters.chain === c.key ? "" : c.key)}
            >
              {c.label}
            </button>
          )}
        </For>
      </div>

      <Show when={status() === "error"}>
        <div class="deals-note" role="alert">
          <p class="deals-note-title">{error()}</p>
          <button type="button" class="bring-btn bring-btn--secondary" onClick={() => void refresh()}>
            {t("retry")}
          </button>
        </div>
      </Show>
      <Show when={status() === "loading"}>
        <p class="deals-note" role="status">
          {t("loading")}
        </p>
      </Show>

      <Show when={status() === "ready"}>
        <div class="deals-body">
          <Show
            when={data() && data()!.maps.webKey}
            fallback={<p class="deals-note deals-note--warn">{t("mapKeyMissing")}</p>}
          >
            <div class="deals-map">
              <div class="deals-map-host" ref={mapHost} aria-label={t("mapTitle")} role="img" />
              <Show when={mapError()}>
                <p class="deals-note deals-note--warn deals-map-fallback">{t("mapKeyMissing")}</p>
              </Show>
            </div>
          </Show>

          <div class="deals-side">
            <ul class="deals-stores" ref={lists} aria-label={t("mapTitle")}>
              <For each={filtered()}>
                {(s) => {
                  const on = () => selectedId() === s.id;
                  return (
                    <li data-store={s.id}>
                      <button
                        type="button"
                        class="deals-store"
                        classList={{ "deals-store--on": on() }}
                        style={{ "--chip-h": String(chainHue(s)) }}
                        aria-current={on() ? "true" : undefined}
                        onClick={() => openStore(s.id)}
                      >
                        <span class="deals-store-letter" aria-hidden="true">
                          {chainInitial(s)}
                        </span>
                        <span class="deals-store-main">
                          <span class="deals-store-name">{s.name}</span>
                          <span class="deals-store-sub">
                            {[distanceLabel(s.distanceM), s.street].filter(Boolean).join(" · ")}
                          </span>
                        </span>
                        <span class="deals-store-badges">
                          {s.listMatches.length > 0 && (
                            <span class="deals-store-badge deals-store-badge--list" aria-label={t("mapMyListChip")}>
                              {s.listMatches.length}
                            </span>
                          )}
                          {s.clearances.length > 0 && (
                            <span class="deals-store-badge deals-store-badge--cl" aria-label={t("mapClearanceChip")}>
                              {s.clearances.length}
                            </span>
                          )}
                        </span>
                      </button>
                    </li>
                  );
                }}
              </For>
            </ul>
            <Show when={filtered().length === 0}>
              <p class="deals-note">{t("mapEmpty")}</p>
            </Show>
          </div>

          <Show when={selected()}>
            {(s) => (
              <aside class="deals-panel" aria-label={s().name}>
                <header class="deals-panel-head">
                  <span class="deals-store-letter deals-store-letter--lg" aria-hidden="true">
                    {chainInitial(s())}
                  </span>
                  <div class="deals-panel-title">
                    <h2>{s().name}</h2>
                    <p>{[s().chain, s().street, distanceLabel(s().distanceM)].filter(Boolean).join(" · ")}</p>
                  </div>
                  <button
                    type="button"
                    class="deals-panel-close"
                    aria-label={t("mapClosePanel", { name: s().name })}
                    onClick={() => setSelectedId(null)}
                  >
                    ×
                  </button>
                </header>
                <a class="deals-directions" href={directionsUrl(s())} target="_blank" rel="noreferrer">
                  🧭 {t("mapDirections")}
                </a>
                <Show when={s().listMatches.length > 0}>
                  <h3 class="deals-panel-sec">{t("mapListTitle")}</h3>
                  <ul class="deals-list">
                    <For each={s().listMatches}>
                      {(m) => (
                        <>
                          <li class="deals-matched">
                            <span class="deals-matched-item">{m.item}</span>
                          </li>
                          <DealRow deal={m.deal} />
                        </>
                      )}
                    </For>
                  </ul>
                </Show>
                <Show when={s().clearances.length > 0}>
                  <h3 class="deals-panel-sec">{t("mapClearancesTitle")}</h3>
                  <ul class="deals-list">
                    <For each={s().clearances}>{(d) => <DealRow deal={d} />}</For>
                  </ul>
                </Show>
                <Show when={s().listMatches.length === 0 && s().clearances.length === 0}>
                  <p class="deals-note">{t("mapHint")}</p>
                </Show>
              </aside>
            )}
          </Show>
        </div>
      </Show>

      <Show when={data() && data()!.failures.length > 0}>
        <p class="deals-note deals-note--foot" role="status">
          {t("mapFailureNote", { src: data()!.failures.map((f) => (f === "flyer" ? t("mapFlyer") : t("mapClearanceChip"))).join(", ") })}
        </p>
      </Show>
    </div>
  );
}
