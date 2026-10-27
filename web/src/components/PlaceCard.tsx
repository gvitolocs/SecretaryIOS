// A place in the chat (`place` blocks, a proactive reminder's `meta.place`,
// shared locations): title, address, time, a static mini-map and the
// distance. Without coordinates it asks the local geocoder (`/v1/maps/geocode`);
// not found -> "Search in Maps", opened only on the owner's click. The card
// opens the place screen (/map/place).

import { useNavigate } from "@solidjs/router";
import { createMemo, createSignal, onCleanup, onMount, Show } from "solid-js";
import { geocode, mapsConfig } from "../api/maps";
import { locale, t } from "../i18n";
import {
  DEFAULT_HOME,
  distanceM,
  formatDistance,
  formatMinutes,
  googleSearchUrl,
  hasPoint,
  placeRoute,
  placeTimeLabel,
  type LatLng,
  type PlaceInfo,
} from "../lib/mapKit";
import { LucinaMap } from "./LucinaMap";
import "../styles/lucinaMap.css";

function useOptionalNavigate(): ((path: string) => void) | null {
  try {
    return useNavigate();
  } catch {
    return null;
  }
}

/** The browser position, only when the owner already allowed it (never prompts from a card). */
async function grantedPosition(): Promise<LatLng | null> {
  try {
    if (typeof navigator === "undefined" || !navigator.geolocation || !navigator.permissions?.query) return null;
    const status = await navigator.permissions.query({ name: "geolocation" });
    if (status.state !== "granted") return null;
    return await new Promise((resolve) =>
      navigator.geolocation.getCurrentPosition(
        (pos) => resolve({ lat: pos.coords.latitude, lng: pos.coords.longitude }),
        () => resolve(null),
        { maximumAge: 300_000, timeout: 10_000 },
      ),
    );
  } catch {
    return null;
  }
}

type Lookup = "known" | "locating" | "found" | "notfound";

export function PlaceCard(props: { place: PlaceInfo; class?: string }) {
  const navigate = useOptionalNavigate();
  const [point, setPoint] = createSignal<LatLng | null>(hasPoint(props.place) ? { lat: props.place.lat, lng: props.place.lng } : null);
  const [lookup, setLookup] = createSignal<Lookup>(point() ? "known" : "locating");
  const [mapShown, setMapShown] = createSignal(true);
  const [origin, setOrigin] = createSignal<{ at: LatLng; you: boolean } | null>(null);
  let alive = true;
  onCleanup(() => {
    alive = false;
  });

  onMount(() => {
    void (async () => {
      const cfg = await mapsConfig();
      const home = cfg?.home ?? DEFAULT_HOME;
      if (!alive) return;
      setOrigin({ at: home, you: false });
      if (!point()) {
        const q = props.place.address || props.place.title;
        let hit: LatLng | null = null;
        try {
          hit = (await geocode(q, home))[0] ?? null;
        } catch {
          hit = null;
        }
        if (!alive) return;
        if (hit) {
          setPoint({ lat: hit.lat, lng: hit.lng });
          setLookup("found");
        } else setLookup("notfound");
      }
      const you = await grantedPosition();
      if (alive && you) setOrigin({ at: you, you: true });
    })();
  });

  const time = createMemo(() => (props.place.when ? placeTimeLabel(props.place.when, props.place.end, locale()) : ""));
  const distance = createMemo(() => {
    const p = point();
    const o = origin();
    if (!p || !o) return "";
    const d = formatDistance(distanceM(o.at, p), locale());
    return o.you ? t("maps.fromYou", { d }) : t("maps.fromHome", { d });
  });
  const route = createMemo(() => placeRoute({ ...props.place, ...(point() ?? {}) }));
  const travel = createMemo(() => {
    const p = props.place;
    if (!p.travelMin || !p.travelMode) return "";
    return `${t(`maps.mode${p.travelMode}`)} · ${p.travelApprox ? "≈ " : ""}${formatMinutes(p.travelMin)}`;
  });

  function open(e: MouseEvent) {
    if (!navigate || e.defaultPrevented || e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(route());
  }

  const head = () => (
    <div class="place-card-head">
      <span class="place-card-icon" aria-hidden="true">
        {props.place.kind === "event" ? "📅" : "📍"}
      </span>
      <span class="place-card-text">
        <span class="place-card-title">{props.place.title}</span>
        <Show when={props.place.address && props.place.address !== props.place.title}>
          <span class="place-card-line">{props.place.address}</span>
        </Show>
        <Show when={time()}>
          <span class="place-card-line place-card-time">{time()}</span>
        </Show>
        <Show when={distance()}>
          <span class="place-card-line place-card-distance">{distance()}</span>
        </Show>
        <Show when={travel()}>
          <span class="place-card-line place-card-travel">{travel()}</span>
        </Show>
        <Show when={lookup() === "locating"}>
          <span class="place-card-line" role="status">
            {t("maps.locating")}
          </span>
        </Show>
      </span>
    </div>
  );

  return (
    <Show
      when={lookup() !== "notfound"}
      fallback={
        <div class={`place-card ${props.class ?? ""}`} data-state="notfound">
          <a class="place-card-notfound-link" href={route()} onClick={open}>{head()}</a>
          <p class="place-card-missing">
            <span>{t("maps.notFound")}</span>
            <a href={googleSearchUrl(props.place.address || props.place.title)} target="_blank" rel="noopener noreferrer">
              {t("maps.searchInMaps")}
            </a>
          </p>
        </div>
      }
    >
      <a class={`place-card ${props.class ?? ""}`} href={route()} onClick={open} title={t("maps.openPlace", { name: props.place.title })}>
        {head()}
        <Show when={point()}>
          {(p) => (
            <div class="place-card-mini" hidden={!mapShown()}>
              <LucinaMap
                interactive={false}
                onAvailable={setMapShown}
                pins={[{ id: "place", lat: p().lat, lng: p().lng, title: props.place.title, tint: "accent" }]}
                label={t("maps.map")}
              />
            </div>
          )}
        </Show>
      </a>
    </Show>
  );
}

export default PlaceCard;
