import type { Calendar } from "@event-calendar/core";
import type { CalEvent, CalProposal, EventDraft } from "../api/calendar";

// Pure helpers of the Calendar tab (views/Calendar.tsx): colours, deep links, the mapping to EventCalendar's events,
// the editor's form <-> request body, and the labels. No DOM, no signals.

// ---------- colours ----------

/** Lucina's own calendar: the app accent (a CSS value, so it follows the shell's theme). */
export const LUCINA_COLOR = "var(--accent)";
export const GOOGLE_COLOR = "#4f7cf0";

/** Accent colours for the other calendars, readable as a 3 px bar and a light tint in both themes. */
export const PALETTE = [
  "#e5534b",
  "#e8912d",
  "#c9a227",
  "#3fb950",
  "#14a3a3",
  "#1f9bcf",
  "#4f7cf0",
  "#a371f7",
  "#db61a2",
  "#a0785a",
] as const;

function hash(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

/** The colour of a calendar: Lucina's is the accent, Google's is blue, the rest a stable pick by name. */
export function calendarColor(name: string, source?: string): string {
  if (source === "lucina") return LUCINA_COLOR;
  if (source === "google" && (name === "Google" || !name)) return GOOGLE_COLOR;
  return PALETTE[hash(name) % PALETTE.length]!;
}

/** The legend name of an event's calendar. */
export function calendarName(e: Pick<CalEvent, "calendar" | "source">): string {
  if (e.source === "lucina") return "Lucina";
  return e.calendar?.trim() || (e.source === "google" ? "Google" : e.source === "iphone" ? "iPhone" : String(e.source));
}

// ---------- deep links ----------

/** The Calendar route that opens one event (`at`: its start, so the right day loads even before it is found). */
export function eventRoute(id: string, atMs?: number): string {
  const q = new URLSearchParams({ event: id });
  if (atMs !== undefined && Number.isFinite(atMs)) q.set("at", String(Math.round(atMs)));
  return `/calendar?${q.toString()}`;
}

/** Google Calendar's `eid` (base64url of "<event id> <calendar email>") → the event id, or null. */
export function googleEidToId(eid: string): string | null {
  try {
    const b64 = eid.replace(/-/g, "+").replace(/_/g, "/");
    const padded = b64 + "=".repeat((4 - (b64.length % 4)) % 4);
    const text = atob(padded);
    const id = text.split(" ")[0]?.trim() ?? "";
    return /^[A-Za-z0-9_@.-]{1,1024}$/.test(id) ? id : null;
  } catch {
    return null;
  }
}

/** A calendar.google.com link to one event (as Lucina's "Added to your calendar" replies carry) → its in-app route. */
export function googleCalendarLinkRoute(href: string): string | null {
  let url: URL;
  try {
    url = new URL(href);
  } catch {
    return null;
  }
  if (url.protocol !== "https:" || url.hostname !== "calendar.google.com") return null;
  const eid = url.searchParams.get("eid");
  if (!eid) return null;
  const id = googleEidToId(eid);
  return id ? eventRoute(`google:${id}`) : null;
}

/** The web app's own origins: this page's, and the production one (links Lucina writes into chat replies). */
const APP_ORIGINS = ["https://getsecretary.tech"];

/**
 * A chat link that opens an event in the Calendar tab: an app link to `/calendar` (this origin or
 * getsecretary.tech, e.g. Lucina's "[Open in calendar](https://getsecretary.tech/calendar?event=lucina%3Aev_abc)"),
 * or a Google Calendar event link. Returns the in-app route, or null for any other link.
 */
export function calendarLinkRoute(href: string, origin = typeof window !== "undefined" ? window.location.origin : ""): string | null {
  let url: URL;
  try {
    url = new URL(href);
  } catch {
    return null;
  }
  if ((url.origin === origin || APP_ORIGINS.includes(url.origin)) && url.pathname === "/calendar") {
    return `/calendar${url.search}`;
  }
  return googleCalendarLinkRoute(href);
}

/** What a "Coming up" notification needs: its category, route and data. */
export interface CalendarNotice {
  category: string;
  route?: string;
  data?: Record<string, unknown>;
}

/**
 * The Calendar route a calendar notification opens: the server's own `/calendar?…` route, else (notifications stored
 * before it sent one) the merged id rebuilt from `data`: the iPhone's raw EventKit id becomes `iphone:<id>@<start>`,
 * a `lucina:`/`google:` id is used as is. Null for other notifications.
 */
export function calendarRoute(n: CalendarNotice): string | null {
  if (n.category !== "calendar") return null;
  if (typeof n.route === "string" && n.route.startsWith("/calendar")) return n.route;
  const id = n.data?.eventId;
  const start = n.data?.start;
  if (typeof id !== "string" || !id.trim() || id.length > 1100) return null;
  const at = typeof start === "number" && Number.isFinite(start) ? start : undefined;
  const merged = /^(lucina|google|iphone|ics):/.test(id) || at === undefined ? id : `iphone:${id}@${at}`;
  return eventRoute(merged, at);
}

/** The provider part of an id, for matching an id from a notification against the loaded events. */
export function idCore(id: string): string {
  return id.replace(/^(lucina|google|iphone|ics):/, "").replace(/@\d{10,}$/, "");
}

/**
 * The event a deep link names: the exact id; else one starting at `at` whose provider id matches (an iPhone copy
 * of a Google event, or the other way round); else the only event starting at `at`.
 */
export function matchDeepLink(events: CalEvent[], id: string, at?: number): CalEvent | null {
  const exact = events.find((e) => e.id === id);
  if (exact) return exact;
  if (at === undefined) {
    const core = idCore(id);
    return events.find((e) => idCore(e.id) === core) ?? null;
  }
  const same = events.filter((e) => e.startMs === at);
  const core = idCore(id);
  const byCore = same.find((e) => {
    const c = idCore(e.id);
    return c === core || core.includes(c) || c.includes(core);
  });
  if (byCore) return byCore;
  return same.length === 1 ? same[0]! : null;
}

// ---------- notes, meeting links ----------

const MEETING_HOST = /(^|\.)(meet\.google\.com|zoom\.us|teams\.microsoft\.com|teams\.live\.com|webex\.com|whereby\.com)$/i;

/** The first video-call link in a text (Meet, Zoom, Teams, Webex, Whereby). */
export function findMeetingUrl(text: string | undefined): string | null {
  if (!text) return null;
  for (const m of text.matchAll(/https:\/\/[^\s<>"')\]]+/g)) {
    const raw = m[0].replace(/[.,;:!?]+$/, "");
    try {
      if (MEETING_HOST.test(new URL(raw).hostname)) return raw;
    } catch {
      // Not a URL.
    }
  }
  return null;
}

/** The event's video-call link: the server's, else one found in its place, notes or link. */
export function meetingUrl(e: Pick<CalEvent, "conferenceUrl" | "location" | "description" | "url">): string | null {
  return e.conferenceUrl || findMeetingUrl(e.location) || findMeetingUrl(e.description) || findMeetingUrl(e.url);
}

const ENTITIES: Record<string, string> = { amp: "&", lt: "<", gt: ">", quot: '"', "#39": "'", apos: "'", nbsp: " " };

/** Notes as plain text: Google descriptions can be HTML. Line breaks stay, tags go, basic entities decode. */
export function plainNotes(text: string | undefined): string {
  if (!text) return "";
  return text
    .replace(/<br\s*\/?>/gi, "\n")
    .replace(/<\/(p|div|li|h\d)>/gi, "\n")
    .replace(/<[^>]+>/g, "")
    .replace(/&(amp|lt|gt|quot|#39|apos|nbsp);/g, (_, k: string) => ENTITIES[k] ?? "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

// ---------- EventCalendar mapping ----------

export interface EcExtra {
  ev?: CalEvent;
  proposal?: CalProposal;
}

/** One server event as an EventCalendar event: Dates for timed events, the dates for all-day ones. */
export function toEcEvent(e: CalEvent, now = Date.now()): Calendar.EventInput {
  const color = calendarColor(calendarName(e), e.source);
  const classes = ["cal-ev", `cal-ev--${e.source}`];
  if (!e.editable) classes.push("cal-ev--ro");
  if (e.endMs <= now) classes.push("cal-ev--past");
  return {
    id: e.id,
    allDay: e.allDay,
    start: e.allDay ? e.start.slice(0, 10) : new Date(e.startMs),
    end: e.allDay ? e.end.slice(0, 10) : new Date(e.endMs),
    title: e.title,
    editable: e.editable,
    startEditable: e.editable,
    durationEditable: e.editable,
    classNames: classes,
    styles: [`--cal-color: ${color}`],
    extendedProps: { ev: e } satisfies EcExtra,
  };
}

/** The id under which a pending proposal is shown. */
export const proposalEventId = (id: string) => `proposal:${id}`;

/** A pending proposal as a dashed event (or null when its times don't parse). */
export function proposalToEc(p: CalProposal): Calendar.EventInput | null {
  const ev = p.event;
  const allDay = ev.allDay ?? /^\d{4}-\d{2}-\d{2}$/.test(ev.start);
  if (allDay) {
    const start = ev.start.slice(0, 10);
    const end = ev.end?.slice(0, 10) || addDaysYmd(start, 1);
    return proposalInput(p, { allDay: true, start, end });
  }
  const s = Date.parse(ev.start);
  if (Number.isNaN(s)) return null;
  const e = ev.end ? Date.parse(ev.end) : s + 3_600_000;
  return proposalInput(p, { allDay: false, start: new Date(s), end: new Date(Number.isNaN(e) ? s + 3_600_000 : e) });
}

function proposalInput(p: CalProposal, when: { allDay: boolean; start: Date | string; end: Date | string }): Calendar.EventInput {
  return {
    id: proposalEventId(p.id),
    ...when,
    title: p.event.title,
    editable: false,
    classNames: ["cal-ev", "cal-ev--proposal"],
    styles: [`--cal-color: ${LUCINA_COLOR}`],
    extendedProps: { proposal: p } satisfies EcExtra,
  };
}

// ---------- dates ----------

const pad = (n: number) => String(n).padStart(2, "0");

/** A local date as `YYYY-MM-DD` (for <input type=date> and all-day events). */
export function localYmd(d: Date): string {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** A local time as `HH:MM` (for <input type=time>). */
export function localHm(d: Date): string {
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** `YYYY-MM-DD` plus `n` days. */
export function addDaysYmd(ymd: string, n: number): string {
  const [y, m, d] = ymd.split("-").map(Number) as [number, number, number];
  return localYmd(new Date(y, m - 1, d + n, 12));
}

/** Local date + time strings → a Date (invalid → null). */
export function fromLocal(ymd: string, hm: string): Date | null {
  const dm = /^(\d{4})-(\d{2})-(\d{2})$/.exec(ymd);
  const tm = /^(\d{1,2}):(\d{2})$/.exec(hm);
  if (!dm || !tm) return null;
  const d = new Date(Number(dm[1]), Number(dm[2]) - 1, Number(dm[3]), Number(tm[1]), Number(tm[2]), 0, 0);
  return Number.isNaN(d.getTime()) ? null : d;
}

/** The query of `GET /v1/calendar/events` for a visible range. */
export function rangeQuery(from: Date, to: Date): string {
  return `from=${encodeURIComponent(from.toISOString())}&to=${encodeURIComponent(to.toISOString())}`;
}

// ---------- the editor ----------

export interface EventForm {
  title: string;
  allDay: boolean;
  startDate: string;
  startTime: string;
  /** Inclusive for all-day events (the form shows the last day, the server wants the day after). */
  endDate: string;
  endTime: string;
  location: string;
  notes: string;
  video: string;
}

/** A new event's form for a time range (a tapped slot, a dragged range, the + button). */
export function formForRange(start: Date, end: Date, allDay: boolean): EventForm {
  const lastDay = allDay ? new Date(end.getTime() - 1) : end;
  return {
    title: "",
    allDay,
    startDate: localYmd(start),
    startTime: localHm(start),
    endDate: localYmd(allDay ? (lastDay < start ? start : lastDay) : end),
    endTime: localHm(end),
    location: "",
    notes: "",
    video: "",
  };
}

/** The form of an existing event. */
export function formForEvent(e: CalEvent): EventForm {
  if (e.allDay) {
    const start = e.start.slice(0, 10);
    const endExcl = e.end.slice(0, 10) || addDaysYmd(start, 1);
    const last = addDaysYmd(endExcl, -1);
    return {
      ...blankText(e),
      allDay: true,
      startDate: start,
      startTime: "09:00",
      endDate: last < start ? start : last,
      endTime: "10:00",
    };
  }
  const s = new Date(e.startMs);
  const en = new Date(e.endMs);
  return { ...blankText(e), allDay: false, startDate: localYmd(s), startTime: localHm(s), endDate: localYmd(en), endTime: localHm(en) };
}

function blankText(e: CalEvent) {
  return {
    title: e.title === "(no title)" ? "" : e.title,
    location: e.location ?? "",
    notes: plainNotes(e.description),
    video: e.conferenceUrl ?? "",
  };
}

/** The start for a new event from the + button: the next full hour today, or 09:00 on the day being looked at. */
export function defaultStart(viewDate: Date, now = new Date()): Date {
  if (localYmd(viewDate) === localYmd(now)) {
    const d = new Date(now);
    d.setMinutes(0, 0, 0);
    d.setHours(d.getHours() + 1);
    return d;
  }
  return new Date(viewDate.getFullYear(), viewDate.getMonth(), viewDate.getDate(), 9, 0, 0, 0);
}

export type FormError = "needTitle" | "needEndAfterStart";

/** The form → the request body (times as UTC instants, all-day as dates with the exclusive end), or what's wrong. */
export function draftFromForm(f: EventForm): { draft: EventDraft } | { error: FormError } {
  const title = f.title.trim();
  if (!title) return { error: "needTitle" };
  const extra: Partial<EventDraft> = {
    location: f.location.trim(),
    description: f.notes.trim(),
  };
  const video = f.video.trim();
  if (video) extra.conferenceUrl = video;
  if (f.allDay) {
    if (!/^\d{4}-\d{2}-\d{2}$/.test(f.startDate) || !/^\d{4}-\d{2}-\d{2}$/.test(f.endDate) || f.endDate < f.startDate) {
      return { error: "needEndAfterStart" };
    }
    return { draft: { title, allDay: true, start: f.startDate, end: addDaysYmd(f.endDate, 1), ...extra } };
  }
  const s = fromLocal(f.startDate, f.startTime);
  const e = fromLocal(f.endDate, f.endTime);
  if (!s || !e || e.getTime() <= s.getTime()) return { error: "needEndAfterStart" };
  return { draft: { title, allDay: false, start: s.toISOString(), end: e.toISOString(), ...extra } };
}

/** Moving the start in the form keeps the event's length (end follows). */
export function shiftEnd(prev: EventForm, next: EventForm): EventForm {
  if (prev.allDay !== next.allDay) return next;
  if (next.allDay) {
    if (prev.startDate === next.startDate) return next;
    const days = daysBetween(prev.startDate, prev.endDate);
    return { ...next, endDate: addDaysYmd(next.startDate, Math.max(0, days)) };
  }
  const ps = fromLocal(prev.startDate, prev.startTime);
  const pe = fromLocal(prev.endDate, prev.endTime);
  const ns = fromLocal(next.startDate, next.startTime);
  if (!ps || !pe || !ns || ps.getTime() === ns.getTime()) return next;
  const len = Math.max(15 * 60_000, pe.getTime() - ps.getTime());
  const ne = new Date(ns.getTime() + len);
  return { ...next, endDate: localYmd(ne), endTime: localHm(ne) };
}

function daysBetween(a: string, b: string): number {
  const [ya, ma, da] = a.split("-").map(Number) as [number, number, number];
  const [yb, mb, db] = b.split("-").map(Number) as [number, number, number];
  return Math.round((Date.UTC(yb, mb - 1, db) - Date.UTC(ya, ma - 1, da)) / 86_400_000);
}

/** A moved/resized EventCalendar event → the PATCH body (all-day: dates, end exclusive). */
export function timesOf(start: Date, end: Date | null, allDay: boolean): Pick<EventDraft, "start" | "end" | "allDay"> {
  if (allDay) {
    const s = localYmd(start);
    const e = end ? localYmd(end) : addDaysYmd(s, 1);
    return { allDay: true, start: s, end: e > s ? e : addDaysYmd(s, 1) };
  }
  const e = end ?? new Date(start.getTime() + 3_600_000);
  return { allDay: false, start: start.toISOString(), end: e.toISOString() };
}

// ---------- labels ----------

const dayFmt = (loc: string) => new Intl.DateTimeFormat(loc, { weekday: "short", day: "numeric", month: "short" });
const timeFmt = (loc: string) => new Intl.DateTimeFormat(loc, { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });

function ymdDate(ymd: string): Date {
  const [y, m, d] = ymd.slice(0, 10).split("-").map(Number) as [number, number, number];
  return new Date(y, m - 1, d, 12);
}

/** "Fri 30 Oct · 13:30–14:30", "Fri 30 Oct, 23:00 – Sat 31 Oct, 01:00", all-day: "Fri 30 Oct" or "Fri 30 Oct – Sun 1 Nov". */
export function whenLabel(e: Pick<CalEvent, "allDay" | "start" | "end" | "startMs" | "endMs">, loc: string): string {
  const day = dayFmt(loc);
  if (e.allDay) {
    const first = ymdDate(e.start);
    const last = ymdDate(addDaysYmd(e.end.slice(0, 10), -1));
    return last.getTime() > first.getTime() ? `${day.format(first)} – ${day.format(last)}` : day.format(first);
  }
  const s = new Date(e.startMs);
  const en = new Date(e.endMs);
  const time = timeFmt(loc);
  if (localYmd(s) === localYmd(en) || (en.getTime() - s.getTime() <= 0)) {
    return `${day.format(s)} · ${time.format(s)}–${time.format(en)}`;
  }
  return `${day.format(s)}, ${time.format(s)} – ${day.format(en)}, ${time.format(en)}`;
}

/** When a pending proposal is, as `whenLabel` says it (the raw start when its times don't parse). */
export function proposalWhen(p: CalProposal, loc: string): string {
  const ev = p.event;
  const allDay = ev.allDay ?? /^\d{4}-\d{2}-\d{2}$/.test(ev.start);
  if (allDay) {
    const start = ev.start.slice(0, 10);
    const end = ev.end?.slice(0, 10) || addDaysYmd(start, 1);
    return whenLabel({ allDay: true, start, end, startMs: 0, endMs: 0 }, loc);
  }
  const s = Date.parse(ev.start);
  if (Number.isNaN(s)) return ev.start;
  const e = ev.end ? Date.parse(ev.end) : NaN;
  return whenLabel({ allDay: false, start: ev.start, end: ev.end ?? "", startMs: s, endMs: Number.isNaN(e) ? s + 3_600_000 : e }, loc);
}

/** "Fri 30 Oct, 14:00" (a moved event's new start). */
export function startLabel(start: Date, allDay: boolean, loc: string): string {
  return allDay ? dayFmt(loc).format(start) : `${dayFmt(loc).format(start)}, ${timeFmt(loc).format(start)}`;
}

/** The parts of a duration: minutes, hours and minutes, or whole days for all-day events. */
export function durationParts(e: Pick<CalEvent, "allDay" | "start" | "end" | "startMs" | "endMs">):
  | { days: number }
  | { h: number; m: number } {
  if (e.allDay) return { days: Math.max(1, daysBetween(e.start.slice(0, 10), e.end.slice(0, 10))) };
  const min = Math.max(0, Math.round((e.endMs - e.startMs) / 60_000));
  return { h: Math.floor(min / 60), m: min % 60 };
}
