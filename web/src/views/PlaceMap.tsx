import { useLocation, useNavigate } from "@solidjs/router";
import { createEffect, createMemo, createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { geocode, mapRoutes, mapsConfig } from "../api/maps";
import { RouteModeIcon } from "../components/Icons";
import { LucinaMap } from "../components/LucinaMap";
import { locale, t } from "../i18n";
import {
  appleMapsUrl,
  defaultRouteMode,
  localTime,
  type RouteMode,
  DEFAULT_HOME,
  distanceM,
  formatDistance,
  formatMinutes,
  googleDirectionsUrl,
  googleSearchUrl,
  hasPoint,
  placeFromQuery,
  placeTimeLabel,
  travelEstimates,
  type LatLng,
  type TravelMode,
} from "../lib/mapKit";
import "../styles/lucinaMap.css";

const MODES: { mode: TravelMode; key: "maps.walk" | "maps.bike" | "maps.car" }[] = [
  { mode: "walk", key: "maps.walk" },
  { mode: "bike", key: "maps.bike" },
  { mode: "car", key: "maps.car" },
];

export default function PlaceMapView() {
  const location = useLocation();
  const navigate = useNavigate();
  const place = createMemo(() => placeFromQuery(location.search));
  const [point, setPoint] = createSignal<LatLng | null>(null);
  const [notFound, setNotFound] = createSignal(false);
  const [home, setHome] = createSignal<LatLng>(DEFAULT_HOME);
  const [you, setYou] = createSignal<LatLng | null>(null);
  const [sheetHeight, setSheetHeight] = createSignal(0);
  let sheetObserver: ResizeObserver | undefined;
  const [menu, setMenu] = createSignal(false);
  let alive = true;
  onCleanup(() => {
    alive = false;
    sheetObserver?.disconnect();
  });

  let lookupTick = 0;
  createEffect(() => {
    const p = place();
    const tick = ++lookupTick;
    setPoint(null);
    setNotFound(false);
    void (async () => {
      const cfg = await mapsConfig();
      if (!alive || tick !== lookupTick) return;
      if (cfg?.home) setHome(cfg.home);
      if (!p) return;
      if (hasPoint(p)) {
        setPoint({ lat: p.lat, lng: p.lng });
        return;
      }
      try {
        const hit = (await geocode(p.address || p.title, cfg?.home ?? DEFAULT_HOME))[0];
        if (!alive || tick !== lookupTick) return;
        if (hit) setPoint({ lat: hit.lat, lng: hit.lng });
        else setNotFound(true);
      } catch {
        if (alive && tick === lookupTick) setNotFound(true);
      }
    })();
  });

  const origin = createMemo(() => (you() ? { at: you()!, mine: true } : { at: home(), mine: false }));
  const meters = createMemo(() => {
    const p = point();
    return p ? distanceM(origin().at, p) : null;
  });
  const distance = createMemo(() => {
    const m = meters();
    if (m === null) return "";
    const d = formatDistance(m, locale());
    return origin().mine ? t("maps.fromYou", { d }) : t("maps.fromHome", { d });
  });
  const estimates = createMemo(() => {
    const m = meters();
    return m === null ? null : travelEstimates(m);
  });
  const time = createMemo(() => {
    const p = place();
    return p?.when ? placeTimeLabel(p.when, p.end, locale()) : "";
  });
  const pins = createMemo(() => {
    const p = point();
    const info = place();
    return p && info ? [{ id: "place", lat: p.lat, lng: p.lng, title: info.title, label: info.title, tint: "accent" as const, selected: true }] : [];
  });

  const [chosenMode, setChosenMode] = createSignal<string | null>((() => {
    try { return localStorage.getItem("lucina.map.mode"); } catch { return null; }
  })());
  const routeQuery = createMemo(() => {
    const to = point();
    if (!to) return null;
    const from = origin().at;
    return { from, to, when: place()?.when };
  });
  const [routes] = createResource(routeQuery, q => mapRoutes(q.from, q.to, q.when));
  const availableRoutes = createMemo(() => routes.loading ? [] : routes() ?? []);
  const selectedRoute = createMemo(() => {
    const choices = availableRoutes();
    const preferred = defaultRouteMode(meters() ?? 0, chosenMode());
    return choices.find(r => r.mode === preferred) ?? choices[0];
  });
  const lines = createMemo(() => {
    const line = selectedRoute()?.polyline;
    return line && line.length > 1 ? [line] : [];
  });
  function selectMode(mode: RouteMode) {
    setChosenMode(mode);
    try { localStorage.setItem("lucina.map.mode", mode); } catch { /* Storage may be disabled in WebViews. */ }
  }

  function back() {
    if (window.history.length > 1) window.history.back();
    else navigate("/");
  }

  return (
    <div class="place-screen">
      <div class="place-screen-map">
        <Show
          when={pins().length}
          fallback={
            <p class="place-screen-nomap" role="status">
              {notFound() ? t("maps.notFound") : place() ? t("maps.locating") : t("maps.missing")}
            </p>
          }
        >
          <LucinaMap
            pins={pins()}
            lines={lines()}
            insets={{ bottom: sheetHeight() + 12 }}
            showsUserLocation
            fitWithUser
            onUserLocation={setYou}
            label={t("maps.map")}
            fallback={<p class="place-screen-nomap">{t("maps.noTiles")}</p>}
          />
        </Show>
      </div>

      <button type="button" class="place-screen-back" onClick={back} aria-label={t("maps.back")}>
        <span aria-hidden="true">‹</span>
        <span>{t("maps.back")}</span>
      </button>

      <Show when={place()}>
        {(p) => (
          <section class="place-sheet" aria-label={p().title} ref={el => {
            sheetObserver?.disconnect();
            if (typeof ResizeObserver !== "undefined") {
              sheetObserver = new ResizeObserver(() => setSheetHeight(el.offsetHeight));
              sheetObserver.observe(el);
            }
          }}>
            <h1>{p().title}</h1>
            <Show when={p().address && p().address !== p().title}>
              <p>{p().address}</p>
            </Show>
            <Show when={time()}>
              <p>{time()}</p>
            </Show>
            <Show when={distance()}>
              <p class="place-distance">{distance()}</p>
            </Show>
            <Show when={availableRoutes().length} fallback={
              <Show when={estimates()}>
                {(e) => <>
                  <ul class="place-eta" aria-label={t("maps.estimates")}>
                    <For each={MODES}>{m => <li><RouteModeIcon mode={m.mode === "car" ? "drive" : m.mode} />{t(m.key, { t: formatMinutes(e()[m.mode]) })}</li>}</For>
                  </ul>
                  <p class="place-route-note">{t("maps.estimateNote")}</p>
                </>}
              </Show>
            }>
              <div class="place-modes" role="group" aria-label={t("maps.travelMode")}>
                <For each={availableRoutes()}>{r =>
                  <button type="button" aria-pressed={selectedRoute()?.mode === r.mode} onClick={() => selectMode(r.mode)}>
                    <RouteModeIcon mode={r.mode} />
                    <span>{t(`maps.mode${r.mode}`)} · {formatMinutes(Math.max(1, Math.round(r.durationS / 60)))}</span>
                    <Show when={r.mode === "drive" && r.staticDurationS !== undefined && r.durationS > r.staticDurationS}>
                      <small>{t("maps.withTraffic")}</small>
                    </Show>
                  </button>
                }</For>
              </div>
              <Show when={selectedRoute()?.leaveAt}>{leave => <p class="place-leave">{t("maps.leaveAt", { time: localTime(leave(), locale()) })}</p>}</Show>
              <Show when={selectedRoute()?.mode === "transit"}>
                <ol class="place-transit">
                  <For each={selectedRoute()?.transit}>{leg => <li>
                    <strong>{leg.line} · {leg.vehicle}</strong>
                    <span>{leg.from} → {leg.to}</span>
                    <Show when={leg.departAt && leg.arriveAt}><time>{localTime(leg.departAt!, locale())} – {localTime(leg.arriveAt!, locale())}</time></Show>
                  </li>}</For>
                </ol>
              </Show>
            </Show>
            <Show
              when={point()}
              fallback={
                <Show when={notFound()}>
                  <p>
                    <a href={googleSearchUrl(p().address || p().title)} target="_blank" rel="noopener">
                      {t("maps.searchInMaps")}
                    </a>
                  </p>
                </Show>
              }
            >
              {(at) => (
                <div class="place-directions">
                  <button type="button" aria-expanded={menu() ? "true" : "false"} aria-haspopup="menu" onClick={() => setMenu((v) => !v)}>
                    {t("maps.directions")}
                  </button>
                  <Show when={menu()}>
                    <div class="place-directions-menu" role="menu">
                      <a role="menuitem" href={appleMapsUrl({ ...at(), title: p().title })} target="_blank" rel="noopener" onClick={() => setMenu(false)}>
                        {t("maps.appleMaps")}
                      </a>
                      <a role="menuitem" href={googleDirectionsUrl(at())} target="_blank" rel="noopener" onClick={() => setMenu(false)}>
                        {t("maps.googleMaps")}
                      </a>
                    </div>
                  </Show>
                </div>
              )}
            </Show>
          </section>
        )}
      </Show>
    </div>
  );
}
