// Deals map (/shopping/map): nearby supermarkets with Lucina's current
// offers. The shared LucinaMap (self-hosted tiles) with chip pins and a home
// pin, fitted to what the filters show; without tiles the store list is the
// whole view.

import { A } from "@solidjs/router";
import { createMemo, createSignal, For, onMount, Show } from "solid-js";
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
import { googleDirectionsUrl, type MapPin } from "../../lib/mapKit";
import { LucinaMap } from "../../components/LucinaMap";
import { shop, t } from "../../state/shopping";
import "../../styles/shopping.css";
import "../../styles/dealsMap.css";

/** The home pin's id (store ids are `<source>:<id>`). */
const HOME_PIN = "home";

const BROWSER_TAG: Record<string, string> = { it: "it-IT", en: "en-GB", da: "da-DK" };

function num(n: number): string {
  return new Intl.NumberFormat(BROWSER_TAG[shop.lang] ?? "da-DK", { minimumFractionDigits: 2, maximumFractionDigits: 2 }).format(n);
}

function day(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return new Intl.DateTimeFormat(BROWSER_TAG[shop.lang] ?? "da-DK", { day: "numeric", month: "short" }).format(d);
}

/** Marker chip: chain initial + offer badge (LucinaMap adds the label and the click). */
function chipElement(store: MapStore): HTMLElement {
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
  return el;
}

function homeElement(): HTMLElement {
  const pin = document.createElement("div");
  pin.className = "deals-home";
  pin.textContent = "🏠";
  return pin;
}

export default function DealsMapView() {
  const [status, setStatus] = createSignal<"loading" | "ready" | "error">("loading");
  const [error, setError] = createSignal<string | null>(null);
  const [data, setData] = createSignal<DealsMapData | null>(null);
  const [filters, setFilters] = createStore({ myList: false, clearance: false, chain: "" });
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  /** null until LucinaMap knows whether tiles are installed. */
  const [mapShown, setMapShown] = createSignal<boolean | null>(null);
  const [focus, setFocus] = createSignal<{ id: string; tick: number } | null>(null);
  let lists: HTMLUListElement | undefined;

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
  const byId = createMemo(() => new Map(stores().map((s) => [s.id, s])));
  const pins = createMemo<MapPin[]>(() => {
    const d = data();
    if (!d) return [];
    const home: MapPin = { id: HOME_PIN, lat: d.home.lat, lng: d.home.lng, title: d.home.label ?? t("mapTitle"), tint: "home" };
    return [
      home,
      ...filtered().map(
        (s): MapPin => ({
          id: s.id,
          lat: s.lat,
          lng: s.lng,
          title: s.name,
          ...(s.street ? { subtitle: s.street } : {}),
          tint: s.clearances.length ? "clearance" : s.listMatches.length ? "accent" : "neutral",
          selected: s.id === selectedId(),
        }),
      ),
    ];
  });

  function renderPin(pin: MapPin): HTMLElement {
    const store = byId().get(pin.id);
    return store ? chipElement(store) : homeElement();
  }

  async function refresh() {
    setStatus("loading");
    setError(null);
    try {
      const d = await shoppingApi.dealsMap();
      setData(d);
      setStatus("ready");
    } catch (e) {
      setError(toApiError(e).message);
      setStatus("error");
    }
  }

  onMount(() => void refresh());

  function onPin(id: string) {
    if (id !== HOME_PIN) openStore(id);
  }

  function openStore(id: string, fromList = false) {
    setSelectedId(id);
    if (fromList) setFocus({ id, tick: (focus()?.tick ?? 0) + 1 });
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
          <Show when={mapShown() !== false} fallback={<p class="deals-note deals-note--warn">{t("mapTilesMissing")}</p>}>
            <div class="deals-map">
              <div class="deals-map-host">
                <LucinaMap
                  pins={pins()}
                  renderPin={renderPin}
                  onSelect={onPin}
                  focus={focus()}
                  onAvailable={setMapShown}
                  label={t("mapTitle")}
                />
              </div>
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
                        onClick={() => openStore(s.id, true)}
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
                <a class="deals-directions" href={googleDirectionsUrl(s())} target="_blank" rel="noreferrer">
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
