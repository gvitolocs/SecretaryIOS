import { createEffect, createMemo, createSignal, For, Index, onCleanup, Show } from "solid-js";
import { mapRoutes, geocode, mapsConfig } from "../../api/maps";
import type { CalEvent, CalProposal, EventDraft } from "../../api/calendar";
import { locale, t } from "../../i18n";
import {
  calendarColor,
  calendarName,
  durationParts,
  meetingUrl,
  plainNotes,
  whenLabel,
} from "../../lib/calendar";
import {
  ALARM_CHOICES,
  alarmParts,
  dayStrip,
  eventPhones,
  leaveAtMs,
  MAX_ALARMS,
  moveTargets,
  noteSegments,
  notesCollapse,
  setAlarm,
  suggestedTravelMin,
  TRAVEL_CHOICES,
  type CalendarTarget,
  type Phone,
} from "../../lib/eventDetail";
import { DEFAULT_HOME, defaultRouteMode, distanceM, formatMinutes, placeRoute, type LatLng, type RouteMode } from "../../lib/mapKit";
import { InAppLink } from "../InAppLink";
import { LinkifiedText } from "../LinkifiedText";
import { LucinaMap } from "../LucinaMap";
import { BellIcon, CalendarIcon, ClockIcon, ExternalIcon, PhoneIcon, RepeatIcon, ShareIcon, UserIcon } from "../Icons";
import "../../styles/lucinaMap.css";

// The body of the event sheet (views/Calendar.tsx owns the Sheet and its buttons), laid out like Apple Calendar's:
// when; phone numbers with Call; the place with a mini map; the video call; the hours around it with the events
// next to it; the settings (calendar, show as, alerts, travel time), changed in place; the notes with tappable
// links, numbers, emails and addresses ("Show more" when long); guests, who added it and where it lives.

const MAX_GUESTS = 10;

export function durationLabel(e: CalEvent): string {
  const d = durationParts(e);
  if ("days" in d) return d.days > 1 ? t("calendar.durationDays", { count: d.days }) : "";
  if (d.h && d.m) return t("calendar.durationHMin", { h: d.h, m: d.m });
  if (d.h) return t("calendar.durationH", { h: d.h });
  return t("calendar.durationMin", { m: d.m });
}

export function sourceLine(e: CalEvent): string {
  if (e.source === "lucina") return t("calendar.sourceLucina");
  if (e.source === "google") return t("calendar.sourceGoogle");
  if (e.source === "iphone") return t("calendar.sourceIphone");
  return t("calendar.sourceIcs", { name: calendarName(e) });
}

/** "1 hour before", "At time of event". */
export function alarmLabel(min: number): string {
  const p = alarmParts(min);
  if ("at" in p) return t("calendar.alertAtStart");
  const key = { min: "calendar.alertMin", h: "calendar.alertH", d: "calendar.alertD", w: "calendar.alertW" } as const;
  return t(key[p.unit], { count: p.count });
}

function travelLabel(min: number): string {
  const h = Math.floor(min / 60);
  const m = min % 60;
  if (h && m) return t("calendar.durationHMin", { h, m });
  if (h) return t("calendar.durationH", { h });
  return t("calendar.durationMin", { m });
}

function PlaceIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 21s-6.5-5.6-6.5-11a6.5 6.5 0 0 1 13 0c0 5.4-6.5 11-6.5 11z" />
      <circle cx="12" cy="10" r="2.4" />
    </svg>
  );
}

function VideoIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="3" y="6.5" width="12.5" height="11" rx="2.5" />
      <path d="m15.5 10.5 5-3v9l-5-3z" />
    </svg>
  );
}

function HandIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M8 13V5.5a1.5 1.5 0 0 1 3 0V11m0-6.5V4a1.5 1.5 0 0 1 3 0v7m0-5.5a1.5 1.5 0 0 1 3 0V12m0-3.5a1.5 1.5 0 0 1 3 0V15a6 6 0 0 1-6 6h-1.2a6 6 0 0 1-4.6-2.2L4.6 15.6a1.5 1.5 0 0 1 2.2-2L8 15" />
    </svg>
  );
}

function CarIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M5 16.5V12l1.8-4.6A2 2 0 0 1 8.7 6h6.6a2 2 0 0 1 1.9 1.4L19 12v4.5" />
      <rect x="3.5" y="11.5" width="17" height="5.5" rx="1.5" />
      <path d="M6 17v2m12-2v2" />
      <circle cx="7.5" cy="14.2" r=".8" />
      <circle cx="16.5" cy="14.2" r=".8" />
    </svg>
  );
}

const longDate = (loc: string) => new Intl.DateTimeFormat(loc, { weekday: "long", day: "numeric", month: "long", year: "numeric" });
const hm = (loc: string) => new Intl.DateTimeFormat(loc, { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });

/** "14:00–15:30" on one day; the full "when" across midnight. */
function timeRange(e: CalEvent): string {
  const s = new Date(e.startMs);
  const en = new Date(e.endMs);
  if (s.toDateString() !== en.toDateString() && en.getTime() - s.getTime() > 0) return whenLabel(e, locale());
  const f = hm(locale());
  return `${f.format(s)}–${f.format(en)}`;
}

/** The browser position, only when the owner already allowed it (never prompts from a sheet). */
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

function PhoneRow(props: { phone: Phone }) {
  const [copied, setCopied] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(timer));
  async function share() {
    const text = props.phone.text;
    try {
      if (typeof navigator.share === "function") {
        await navigator.share({ text });
        return;
      }
    } catch (e) {
      if (e instanceof DOMException && e.name === "AbortError") return;
    }
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      clearTimeout(timer);
      timer = setTimeout(() => setCopied(false), 2000);
    } catch {
      // No clipboard: nothing to do.
    }
  }
  return (
    <div class="cal-card cal-phone">
      <span class="cal-phone-badge" aria-hidden="true">
        <PhoneIcon />
      </span>
      <a class="cal-phone-number" href={props.phone.href}>
        {props.phone.text}
      </a>
      <a class="pg-btn cal-pill" href={props.phone.href} aria-label={t("calendar.callNumber", { number: props.phone.text })}>
        {t("calendar.call")}
      </a>
      <button type="button" class="cal-round" onClick={() => void share()} aria-label={t("calendar.shareNumber", { number: props.phone.text })}>
        <ShareIcon />
      </button>
      <Show when={copied()}>
        <span class="cal-phone-copied" role="status">
          {t("calendar.copied")}
        </span>
      </Show>
    </div>
  );
}

function PlaceCardMini(props: { e: CalEvent; place: string; point: LatLng | null }) {
  const [mapShown, setMapShown] = createSignal(true);
  const route = createMemo(() =>
    placeRoute({
      title: props.e.title,
      address: props.place,
      ...(props.point ?? {}),
      when: props.e.allDay ? undefined : new Date(props.e.startMs).toISOString(),
      end: props.e.allDay ? undefined : new Date(props.e.endMs).toISOString(),
      eventId: props.e.id,
    }),
  );
  const gmaps = () => `https://www.google.com/maps/search/?api=1&query=${encodeURIComponent(props.place)}`;
  return (
    <div class="cal-card cal-place">
      <InAppLink href={route()} class="cal-place-main" aria-label={t("calendar.openMap", { place: props.place })}>
        <span class="cal-place-text">{props.place}</span>
        <Show when={props.point}>
          {(p) => (
            <span class="cal-place-map" hidden={!mapShown()}>
              <LucinaMap
                interactive={false}
                onAvailable={setMapShown}
                pins={[{ id: "place", lat: p().lat, lng: p().lng, title: props.place, tint: "accent" }]}
                label={t("maps.map")}
              />
            </span>
          )}
        </Show>
      </InAppLink>
      <p class="cal-detail-links">
        <InAppLink href={route()} class="pg-link cal-link">
          {t("calendar.showOnMap")}
        </InAppLink>
        <a href={gmaps()} target="_blank" rel="noopener noreferrer" class="pg-link cal-link">
          {t("calendar.googleMaps")}
        </a>
      </p>
    </div>
  );
}

function DayStripView(props: { e: CalEvent; events: CalEvent[]; onShow?: (e: CalEvent) => void }) {
  const strip = createMemo(() => dayStrip(props.e, props.events));
  const time = () => hm(locale());
  return (
    <Show when={strip()}>
      {(s) => (
        <section class="cal-card cal-strip" aria-label={t("calendar.dayAround")}>
          <div class="cal-strip-grid" style={{ "--hours": String(s().hours.length - 1) }}>
            <For each={s().hours}>
              {(h, i) => (
                <span class="cal-strip-hour" style={{ top: `${(i() / (s().hours.length - 1)) * 100}%` }}>
                  <span class="cal-strip-hour-label">{time().format(new Date(h))}</span>
                </span>
              )}
            </For>
            <div class="cal-strip-lane">
              <For each={s().blocks}>
                {(b) => (
                  <button
                    type="button"
                    class="cal-strip-ev"
                    classList={{ "cal-strip-ev--self": b.self }}
                    style={{
                      top: `${b.top * 100}%`,
                      height: `${b.height * 100}%`,
                      left: `${(b.col / b.cols) * 100}%`,
                      width: `${100 / b.cols}%`,
                      "--cal-color": calendarColor(calendarName(b.ev), b.ev.source),
                    }}
                    disabled={b.self || !props.onShow}
                    aria-current={b.self ? "true" : undefined}
                    onClick={() => props.onShow?.(b.ev)}
                  >
                    <span class="cal-strip-title">{b.ev.title}</span>
                    <span class="cal-strip-when">
                      {time().format(new Date(b.ev.startMs))}–{time().format(new Date(b.ev.endMs))}
                      <Show when={b.ev.location}>{(l) => <> · {l()}</>}</Show>
                    </span>
                  </button>
                )}
              </For>
            </div>
          </div>
        </section>
      )}
    </Show>
  );
}

function Notes(props: { text: string }) {
  const long = createMemo(() => notesCollapse(props.text));
  const [open, setOpen] = createSignal(false);
  const segments = createMemo(() => noteSegments(props.text));
  return (
    <section class="cal-detail-section" aria-label={t("calendar.moreDetails")}>
      <h3 class="cal-detail-h">{t("calendar.moreDetails")}</h3>
      <div class="cal-card cal-notes-card">
        <p class="cal-detail-notes rich" classList={{ "cal-detail-notes--clamped": long() && !open() }} id="cal-notes">
          <Index each={segments()}>
            {(seg) => {
              const s = seg();
              if (s.kind === "text") return s.text;
              if (s.kind === "address") {
                return (
                  <InAppLink href={placeRoute({ title: s.text, address: s.query })} class="cal-note-data">
                    {s.text}
                  </InAppLink>
                );
              }
              if (s.kind === "link") {
                return (
                  <a href={s.href} target="_blank" rel="noopener noreferrer nofollow" class="cal-note-data">
                    {s.text}
                  </a>
                );
              }
              return (
                <a href={s.href} class="cal-note-data">
                  {s.text}
                </a>
              );
            }}
          </Index>
        </p>
        <Show when={long()}>
          <button type="button" class="pg-link cal-more" aria-expanded={open()} aria-controls="cal-notes" onClick={() => setOpen(!open())}>
            {open() ? t("calendar.showLess") : t("calendar.showMore")}
          </button>
        </Show>
      </div>
    </section>
  );
}

export interface EventDetailProps {
  event: CalEvent;
  /** The loaded events (the day strip picks the ones around this one). */
  dayEvents?: CalEvent[];
  /** Google Calendar is connected (the event can move between Lucina's and Google's calendar). */
  googleConnected?: boolean;
  /** Saves a settings change (alerts, show as, travel time); rejects with the reason. */
  onChange?: (patch: Partial<EventDraft>) => Promise<void>;
  /** Moves the event to another calendar. */
  onMove?: (target: CalendarTarget) => Promise<void>;
  /** Opens another event from the day strip. */
  onShowEvent?: (e: CalEvent) => void;
}

export function EventDetail(props: EventDetailProps) {
  const e = () => props.event;
  const color = createMemo(() => calendarColor(calendarName(e()), e().source));
  const notes = createMemo(() => plainNotes(e().description));
  const video = createMemo(() => meetingUrl(e()));
  const guests = createMemo(() => e().attendees ?? []);
  const phones = createMemo(() => eventPhones(e(), notes()));
  const place = createMemo(() => {
    const loc = e().location?.trim();
    // A place that is only a link (a Teams room, a Zoom id) is shown by the video call button.
    return loc && !/^https?:\/\/\S+$/.test(loc) ? loc : null;
  });
  const canEdit = () => e().editable && !!props.onChange;
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");

  // The place's point and the travel time to it (from the browser's position when allowed, else home).
  const [point, setPoint] = createSignal<LatLng | null>(null);
  const [route, setRoute] = createSignal<{ min: number; mode: RouteMode } | null>(null);
  let alive = true;
  onCleanup(() => {
    alive = false;
  });
  let lookedUp = "";
  createEffect(() => {
    const q = place();
    if (!q || q === lookedUp) return;
    lookedUp = q;
    setPoint(null);
    setRoute(null);
    void (async () => {
      try {
        const cfg = await mapsConfig();
        const home = cfg?.home ?? DEFAULT_HOME;
        const hit = (await geocode(q, home))[0];
        if (!alive || lookedUp !== q || !hit) return;
        const to = { lat: hit.lat, lng: hit.lng };
        setPoint(to);
        const from = (await grantedPosition()) ?? home;
        const when = e().allDay ? undefined : new Date(e().startMs).toISOString();
        const routes = await mapRoutes(from, to, when);
        if (!alive || lookedUp !== q) return;
        const mode = defaultRouteMode(distanceM(from, to), readMode());
        const pick = routes.find((r) => r.mode === mode) ?? routes[0];
        if (pick) setRoute({ min: suggestedTravelMin(pick.durationS), mode: pick.mode });
      } catch {
        // No map server: no mini map and no suggestion.
      }
    })();
  });

  async function change(patch: Partial<EventDraft>) {
    if (!props.onChange || busy()) return;
    setBusy(true);
    setError("");
    try {
      await props.onChange(patch);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  async function move(target: CalendarTarget) {
    if (!props.onMove || busy() || target === e().source) return;
    setBusy(true);
    setError("");
    try {
      await props.onMove(target);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  const alarms = createMemo(() => e().alarms ?? []);
  // While the calendar's defaults apply, the rows show them; changing one turns them into the event's own.
  const alarmRows = createMemo(() => {
    const rows: (number | null)[] = [...alarms()];
    if (canEdit() && rows.length < MAX_ALARMS) rows.push(null);
    return rows;
  });
  const moves = createMemo(() => moveTargets(e(), !!props.googleConnected));
  const leaveAt = createMemo(() => leaveAtMs(e()));
  const travelChoices = createMemo(() => {
    const set = new Set<number>(TRAVEL_CHOICES);
    const cur = e().travelMin;
    if (cur) set.add(cur);
    const r = route();
    if (r) set.add(r.min);
    return [...set].sort((a, b) => a - b);
  });

  function setLeave(on: boolean) {
    if (!on) return void change({ leaveAlert: false });
    const travel = e().travelMin || route()?.min;
    if (!travel) return;
    void change(e().travelMin ? { leaveAlert: true } : { leaveAlert: true, travelMin: travel });
  }

  return (
    <div class="cal-detail">
      <div class="cal-detail-head" style={{ "--cal-color": color() }}>
        <p class="cal-detail-date">
          {e().allDay ? whenLabel(e(), locale()) : longDate(locale()).format(new Date(e().startMs))}
        </p>
        <p class="cal-detail-time">
          <Show
            when={!e().allDay}
            fallback={[t("calendar.allDay"), durationLabel(e())].filter(Boolean).join(" · ")}
          >
            <ClockIcon />
            {timeRange(e())} <span class="pg-muted">· {durationLabel(e())}</span>
          </Show>
        </p>
        <p class="cal-detail-cal">
          <span class="cal-dot" style={{ "--cal-color": color() }} aria-hidden="true" />
          <span>{calendarName(e())}</span>
          <Show when={e().recurring}>
            <span class="pg-badge cal-detail-badge">
              <RepeatIcon />
              {t("calendar.repeats")}
            </span>
          </Show>
        </p>
      </div>

      <Show when={phones().length || place()}>
        <section class="cal-detail-section" aria-label={t("calendar.location")}>
          <h3 class="cal-detail-h">{t("calendar.location")}</h3>
          <For each={phones()}>{(p) => <PhoneRow phone={p} />}</For>
          <Show when={place()}>{(loc) => <PlaceCardMini e={e()} place={loc()} point={point()} />}</Show>
        </section>
      </Show>

      <Show when={video()}>
        {(href) => (
          <a class="pg-btn cal-join" href={href()} target="_blank" rel="noopener noreferrer">
            <VideoIcon />
            {t("calendar.joinCall")}
          </a>
        )}
      </Show>

      <DayStripView e={e()} events={props.dayEvents ?? []} onShow={props.onShowEvent} />

      <section class="cal-detail-section" aria-label={t("calendar.settings")}>
        <h3 class="cal-detail-h">{t("calendar.settings")}</h3>
        <div class="cal-card cal-settings" aria-busy={busy()}>
          <label class="cal-set">
            <CalendarIcon />
            <span class="cal-set-name">{t("calendar.calendarField")}</span>
            <Show
              when={moves().length && props.onMove}
              fallback={
                <span class="cal-set-value">
                  <span class="cal-dot" style={{ "--cal-color": color() }} aria-hidden="true" />
                  {calendarName(e())}
                </span>
              }
            >
              <select class="cal-select" value={e().source} disabled={busy()} onChange={(ev) => void move(ev.currentTarget.value as CalendarTarget)}>
                <For each={moves()}>{(m) => <option value={m}>{m === "lucina" ? "Lucina" : "Google"}</option>}</For>
              </select>
            </Show>
          </label>

          <label class="cal-set">
            <HandIcon />
            <span class="cal-set-name">{t("calendar.showAs")}</span>
            <Show when={canEdit()} fallback={<span class="cal-set-value">{e().free ? t("calendar.free") : t("calendar.busy")}</span>}>
              <select class="cal-select" value={e().free ? "free" : "busy"} disabled={busy()} onChange={(ev) => void change({ free: ev.currentTarget.value === "free" })}>
                <option value="busy">{t("calendar.busy")}</option>
                <option value="free">{t("calendar.free")}</option>
              </select>
            </Show>
          </label>

          <Show when={place() && (canEdit() || e().leaveAlert)}>
            <label class="cal-set">
              <BellIcon />
              <span class="cal-set-name">{t("calendar.timeToLeave")}</span>
              <span class="cal-set-value cal-set-value--stack">
                <Show
                  when={canEdit()}
                  fallback={<span>{leaveAt() ? t("calendar.leaveAt", { time: hm(locale()).format(new Date(leaveAt()!)) }) : ""}</span>}
                >
                  <input
                    type="checkbox"
                    class="cal-switch"
                    role="switch"
                    checked={!!e().leaveAlert}
                    disabled={busy() || (!e().leaveAlert && !e().travelMin && !route())}
                    onChange={(ev) => setLeave(ev.currentTarget.checked)}
                  />
                </Show>
                <Show when={e().leaveAlert && leaveAt()}>
                  {(at) => <span class="pg-muted">{t("calendar.leaveAt", { time: hm(locale()).format(new Date(at())) })}</span>}
                </Show>
                <Show when={canEdit() && !e().leaveAlert && !e().travelMin && !route()}>
                  <span class="pg-muted">{t("calendar.leaveNeedsTravel")}</span>
                </Show>
              </span>
            </label>
          </Show>

          <Show when={canEdit()} fallback={
            <For each={alarms()}>
              {(m) => (
                <div class="cal-set">
                  <BellIcon />
                  <span class="cal-set-name">{t("calendar.alert")}</span>
                  <span class="cal-set-value">{e().alarmsDefault ? t("calendar.alertDefault", { label: alarmLabel(m) }) : alarmLabel(m)}</span>
                </div>
              )}
            </For>
          }>
            <Index each={alarmRows()}>
              {(row, i) => (
                <label class="cal-set">
                  <BellIcon />
                  <span class="cal-set-name">{t("calendar.alert")}</span>
                  <select
                    class="cal-select"
                    value={row() === null ? "none" : String(row())}
                    disabled={busy()}
                    onChange={(ev) => {
                      const v = ev.currentTarget.value;
                      void change({ alarms: setAlarm(alarms(), i, v === "none" ? null : Number(v)) });
                    }}
                  >
                    <option value="none">{t("calendar.alertNone")}</option>
                    <For each={[...new Set([...ALARM_CHOICES, ...(row() === null ? [] : [row()!])])].sort((a, b) => a - b)}>
                      {(m) => (
                        <option value={String(m)}>
                          {e().alarmsDefault && row() === m ? t("calendar.alertDefault", { label: alarmLabel(m) }) : alarmLabel(m)}
                        </option>
                      )}
                    </For>
                  </select>
                </label>
              )}
            </Index>
            <Show when={e().alarmsDefault && !alarms().length}>
              <p class="pg-muted cal-set-note">{t("calendar.alertsDefault")}</p>
            </Show>
          </Show>

          <label class="cal-set">
            <CarIcon />
            <span class="cal-set-name">{t("calendar.travelTime")}</span>
            <Show
              when={canEdit()}
              fallback={<span class="cal-set-value">{e().travelMin ? travelLabel(e().travelMin!) : t("calendar.travelNone")}</span>}
            >
              <select
                class="cal-select"
                value={String(e().travelMin ?? 0)}
                disabled={busy()}
                onChange={(ev) => {
                  const v = Number(ev.currentTarget.value);
                  void change(v ? { travelMin: v } : { travelMin: null, leaveAlert: false });
                }}
              >
                <option value="0">{t("calendar.travelNone")}</option>
                <For each={travelChoices()}>
                  {(m) => (
                    <option value={String(m)}>
                      {route()?.min === m
                        ? t("calendar.travelSuggested", { time: formatMinutes(m), mode: t(`maps.mode${route()!.mode}`).toLowerCase() })
                        : travelLabel(m)}
                    </option>
                  )}
                </For>
              </select>
            </Show>
          </label>
        </div>
        <Show when={error()}>
          <p class="cal-detail-error" role="alert">
            {t("calendar.failed", { reason: error() })}
          </p>
        </Show>
      </section>

      <Show when={notes()}>
        <Notes text={notes()} />
      </Show>

      <Show when={guests().length}>
        <section class="cal-detail-section" aria-label={t("calendar.guests")}>
          <h3 class="cal-detail-h">{t("calendar.guests")}</h3>
          <ul class="cal-guests">
            <For each={guests().slice(0, MAX_GUESTS)}>{(g) => <li>{g}</li>}</For>
          </ul>
          <Show when={guests().length > MAX_GUESTS}>
            <p class="pg-muted">{t("calendar.moreGuests", { count: guests().length - MAX_GUESTS })}</p>
          </Show>
        </section>
      </Show>

      <div class="cal-detail-meta">
        <Show when={e().addedBy}>
          {(who) => (
            <p class="cal-detail-row cal-detail-row--quiet">
              <UserIcon />
              <span>{t("calendar.addedBy", { name: who() })}</span>
            </p>
          )}
        </Show>
        <p class="pg-muted">{sourceLine(e())}</p>
        <Show when={e().url && e().source === "google"}>
          <a href={e().url} target="_blank" rel="noopener noreferrer" class="pg-link cal-link">
            {t("calendar.openInGoogle")}
            <ExternalIcon />
          </a>
        </Show>
      </div>
    </div>
  );
}

/** The travel mode the place screen remembers (views/PlaceMap), if any. */
function readMode(): string | null {
  try {
    return localStorage.getItem("lucina.map.mode");
  } catch {
    return null;
  }
}

/** The body of a pending proposal's sheet. */
export function ProposalDetail(props: { proposal: CalProposal; when: string }) {
  const p = () => props.proposal;
  return (
    <div class="cal-detail">
      <p class="cal-detail-cal">
        <span class="cal-dot cal-dot--proposal" aria-hidden="true" />
        <span>{t("calendar.proposed")}</span>
      </p>
      <div class="cal-detail-row">
        <ClockIcon />
        <p class="cal-detail-strong">{props.when}</p>
      </div>
      <Show when={p().event.location}>
        {(loc) => (
          <div class="cal-detail-row">
            <PlaceIcon />
            <p class="cal-detail-strong cal-detail-wrap">{loc()}</p>
          </div>
        )}
      </Show>
      <Show when={plainNotes(p().event.description)}>
        {(n) => (
          <p class="cal-detail-notes">
            <LinkifiedText text={n()} />
          </p>
        )}
      </Show>
      <div class="cal-detail-meta">
        <p class="pg-muted">{t("calendar.proposedBy", { from: p().from })}</p>
        <Show when={p().reason}>{(r) => <p class="pg-muted">{t("calendar.proposalReason", { reason: r() })}</p>}</Show>
      </div>
    </div>
  );
}
