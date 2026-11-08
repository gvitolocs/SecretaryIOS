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
import { navigateRoute, type MapPin } from "../../lib/mapKit";
import { LucinaMap } from "../../components/LucinaMap";
import { Button, Card, EmptyState, ErrorState, ListRow, LoadingState, Page, PageGrid, PageHeader, RowList, useWide } from "../../components/page";
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
  const { wide, track } = useWide();
  const [status, setStatus] = createSignal<"loading" | "ready" | "error">("loading");
  const [error, setError] = createSignal<string | null>(null);
  const [data, setData] = createSignal<DealsMapData | null>(null);
  const [filters, setFilters] = createStore({ myList: false, clearance: false, chain: "" });
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  /** null until LucinaMap knows whether tiles are installed. */
  const [mapShown, setMapShown] = createSignal<boolean | null>(null);
  const [focus, setFocus] = createSignal<{ id: string; tick: number } | null>(null);
  let lists: HTMLElement | undefined;

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

  const Filters = () => (
    <div class={wide() ? "deals-filters deals-filters--wide" : "deals-filters"} role="group" aria-label={t("mapTitle")}>
      <button type="button" class={wide() ? "pg-btn" : "deals-chipbtn"} aria-pressed={filters.myList} onClick={() => toggle("myList")}>
        {t("mapMyListChip")}
      </button>
      <button type="button" class={wide() ? "pg-btn" : "deals-chipbtn"} aria-pressed={filters.clearance} onClick={() => toggle("clearance")}>
        {t("mapClearanceChip")}
      </button>
      <For each={chains()}>
        {(c) => (
          <button
            type="button"
            class={wide() ? "pg-btn" : "deals-chipbtn"}
            aria-pressed={filters.chain === c.key}
            onClick={() => setFilters("chain", filters.chain === c.key ? "" : c.key)}
          >
            {c.label}
          </button>
        )}
      </For>
    </div>
  );

  const MapView = () => (
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
  );

  /** The offers of the selected store, as rows (list matches first, then food waste). */
  const Offers = (p: { store: MapStore }) => (
    <>
      <A
        class={wide() ? "pg-btn deals-directions-wide" : "deals-directions"}
        href={navigateRoute({ title: p.store.name, address: p.store.street ?? undefined, lat: p.store.lat, lng: p.store.lng })}
      >
        🧭 {t("mapDirections")}
      </A>
      <Show when={p.store.listMatches.length > 0}>
        <h3 class="deals-panel-sec">{t("mapListTitle")}</h3>
        <ul class="deals-list">
          <For each={p.store.listMatches}>
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
      <Show when={p.store.clearances.length > 0}>
        <h3 class="deals-panel-sec">{t("mapClearancesTitle")}</h3>
        <ul class="deals-list">
          <For each={p.store.clearances}>{(d) => <DealRow deal={d} />}</For>
        </ul>
      </Show>
      <Show when={p.store.listMatches.length === 0 && p.store.clearances.length === 0}>
        <p class="deals-note">{t("mapHint")}</p>
      </Show>
    </>
  );

  const FailureNote = () => (
    <Show when={data() && data()!.failures.length > 0}>
      <p class={wide() ? "pg-notice" : "deals-note deals-note--foot"} role="status">
        {t("mapFailureNote", { src: data()!.failures.map((f) => (f === "flyer" ? t("mapFlyer") : t("mapClearanceChip"))).join(", ") })}
      </p>
    </Show>
  );

  const PhoneLayout = () => (
    <>
      <header class="deals-head">
        <A class="bring-back-pill deals-back" href="/shopping" aria-label={t("mapBackList")}>
          <span class="bring-chevron-back" aria-hidden="true" />
          <span>{t("mapBackList")}</span>
        </A>
        <h1 class="deals-title">{t("mapTitle")}</h1>
      </header>

      <Filters />

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
            <MapView />
          </Show>

          <div class="deals-side">
            <ul class="deals-stores" ref={(el) => (lists = el)} aria-label={t("mapTitle")}>
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
                <Offers store={s()} />
              </aside>
            )}
          </Show>
        </div>
      </Show>

      <FailureNote />
    </>
  );

  /** The desktop frame: header with a link back to the list, store chips, the stores list left and the map right. */
  const WideLayout = () => (
    <div class="bring-scroll deals-scroll">
      <Page class="deals-wide">
        <PageHeader
          title={t("mapTitle")}
          context={
            <A class="pg-link pg-back" href="/shopping">
              <span aria-hidden="true">‹</span> {t("mapBackList")}
            </A>
          }
        />
        <Filters />
        <FailureNote />
        <Show when={status() === "error"}>
          <ErrorState message={error() ?? ""} retryLabel={t("retry")} onRetry={() => void refresh()} />
        </Show>
        <Show when={status() === "loading"}>
          <LoadingState label={t("loading")} />
        </Show>
        <Show when={status() === "ready"}>
          <PageGrid
            layout="split"
            railLabel={t("mapTitle")}
            rail={
              <Card list>
                <Show when={filtered().length > 0} fallback={<EmptyState text={t("mapEmpty")} />}>
                  <div class="deals-stores-wrap" ref={(el) => (lists = el)}>
                    <RowList label={t("mapTitle")}>
                      <For each={filtered()}>
                        {(s) => {
                          const on = () => selectedId() === s.id;
                          return (
                            <ListRow
                              class={on() ? "deals-row--on" : undefined}
                              leading={
                                <span class="deals-store-letter" style={{ "--chip-h": String(chainHue(s)) }} aria-hidden="true">
                                  {chainInitial(s)}
                                </span>
                              }
                              title={
                                <button
                                  type="button"
                                  class="pg-row-name"
                                  data-store={s.id}
                                  aria-current={on() ? "true" : undefined}
                                  onClick={() => openStore(s.id, true)}
                                >
                                  {s.name}
                                </button>
                              }
                              sub={[distanceLabel(s.distanceM), s.street].filter(Boolean).join(" · ")}
                              meta={
                                <>
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
                                </>
                              }
                            />
                          );
                        }}
                      </For>
                    </RowList>
                  </div>
                </Show>
              </Card>
            }
          >
            <Card list class="deals-map-card">
              <Show when={mapShown() !== false} fallback={<EmptyState text={t("mapTilesMissing")} />}>
                <MapView />
              </Show>
            </Card>
            <Show when={selected()}>
              {(s) => (
                <aside aria-label={s().name}>
                  <Card
                    title={s().name}
                    actions={
                      <Button variant="ghost" aria-label={t("mapClosePanel", { name: s().name })} onClick={() => setSelectedId(null)}>
                        ×
                      </Button>
                    }
                  >
                    <p class="pg-context deals-wide-sub">{[s().chain, s().street, distanceLabel(s().distanceM)].filter(Boolean).join(" · ")}</p>
                    <Offers store={s()} />
                  </Card>
                </aside>
              )}
            </Show>
          </PageGrid>
        </Show>
      </Page>
    </div>
  );

  return (
    <div class="bring deals" ref={track} lang={shop.lang}>
      <Show when={wide()} fallback={<PhoneLayout />}>
        <WideLayout />
      </Show>
    </div>
  );
}
