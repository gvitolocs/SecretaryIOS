import type { CalEvent } from "../api/calendar";
import { linkify } from "./linkify";

// Pure helpers of the event sheet (components/calendar/EventDetail.tsx), the web twin of the iPhone's
// CalendarDetail: phone numbers, the notes' live data (links, phones, emails, addresses), the "Show more" rule,
// the day strip around the event and the alert / travel-time choices. No DOM, no signals. Meeting links stay https:
// the iPhone opens Teams / Zoom from them (universal links); the Mac app hands only https links to the browser.

// ---------- phone numbers ----------

export interface Phone {
  /** As written in the event. */
  text: string;
  /** `tel:+4552200144`. */
  href: string;
}

// `+45 52 20 01 44`, `+39 333 123 4567`, `0045 5220 0144`, `52 20 01 44`, `5220 0144`, `52200144`. Groups are split by
// one space, dash, dot or a pair of parentheses; the digit counts are checked after the match.
const PHONE_RE = /(?:\+|00)\d{1,4}(?:[ .-]?\(?\d{1,5}\)?){2,7}|\b[2-9]\d(?: ?\d\d){3}\b|\b[2-9]\d{3} \d{4}\b/g;
// Neighbours that make a digit run part of something else (a date, a time, a room or order number, a URL, an email).
const NOT_BEFORE = /[\p{L}\d/.:@-]/u;
const NOT_AFTER = /[\p{L}\d/:@]/u;

/** The tel: link of a written number, or null when it isn't one (Danish 8 digits, else +/00 and 8-15 digits). */
export function phoneHref(text: string): string | null {
  const digits = text.replace(/\D/g, "");
  if (text.startsWith("+")) return digits.length >= 8 && digits.length <= 15 ? `tel:+${digits}` : null;
  if (text.startsWith("00")) {
    const rest = digits.slice(2);
    return rest.length >= 8 && rest.length <= 13 ? `tel:+${rest}` : null;
  }
  return digits.length === 8 && /^[2-9]/.test(digits) ? `tel:+45${digits}` : null;
}

/** Phone numbers in a text with their positions, outside links and email addresses. */
export function phoneSpans(text: string): (Phone & { start: number; end: number })[] {
  const blocked = linkSpans(text);
  const out: (Phone & { start: number; end: number })[] = [];
  for (const m of text.matchAll(PHONE_RE)) {
    let raw = m[0];
    const start = m.index ?? 0;
    // A trailing dot ends the sentence, not the number.
    raw = raw.replace(/[.-]+$/, "");
    const end = start + raw.length;
    if (NOT_BEFORE.test(text[start - 1] ?? "") || NOT_AFTER.test(text[end] ?? "")) continue;
    // "5220-014": a dash or dot right after the last digit belongs to another number.
    if (/[.-]\d/.test(text.slice(end, end + 2))) continue;
    if (blocked.some(([a, b]) => start < b && end > a)) continue;
    const href = phoneHref(raw);
    if (href) out.push({ text: raw, href, start, end });
  }
  return out;
}

/** The event's phone numbers: place, title and notes, each number once, at most three. */
export function eventPhones(e: Pick<CalEvent, "location" | "title" | "description">, notes = e.description ?? ""): Phone[] {
  const seen = new Set<string>();
  const out: Phone[] = [];
  for (const text of [e.location ?? "", e.title, notes]) {
    for (const p of phoneSpans(text)) {
      if (seen.has(p.href) || out.length >= 3) continue;
      seen.add(p.href);
      out.push({ text: p.text, href: p.href });
    }
  }
  return out;
}

// ---------- notes with live data ----------

export type NoteSegment =
  | { kind: "text"; text: string }
  | { kind: "link"; text: string; href: string }
  | { kind: "email"; text: string; href: string }
  | { kind: "phone"; text: string; href: string }
  | { kind: "address"; text: string; query: string };

const EMAIL_RE = /\b[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}\b/g;
// "Universitetsbyen 76, 8000 Aarhus C", "Via Roma 12, 00184 Roma", "Skejbygårdsvej 3, 2. tv, 8240 Risskov":
// a street (capitalised words and the small ones between them: "Via dei Mille"), a house number, an optional
// floor/door part, a 4-5 digit postcode and a capitalised town.
const ADDRESS_RE =
  /\p{Lu}[\p{L}'.-]*(?: (?:\p{Lu}[\p{L}'.-]*|(?:di|da|de|del|della|delle|dei|degli|dello|la|le|lo|van|von|der|den|ved|på|i)(?= ))){0,4} \d{1,4}[A-Za-z]?(?:, ?[\p{L}\d. ]{1,20})?, ?\d{4,5} \p{Lu}[\p{L}-]+(?: (?:\p{Lu}\b|\p{Lu}[\p{L}-]+))?/gu;

function linkSpans(text: string): [number, number][] {
  const spans: [number, number][] = [];
  let at = 0;
  for (const s of linkify(text)) {
    if (s.kind === "link") spans.push([at, at + s.text.length]);
    at += s.text.length;
  }
  for (const m of text.matchAll(EMAIL_RE)) spans.push([m.index ?? 0, (m.index ?? 0) + m[0].length]);
  return spans;
}

/** Notes split into plain text and tappable data: links, email addresses, phone numbers and street addresses. */
export function noteSegments(text: string): NoteSegment[] {
  type Hit = { start: number; end: number; seg: Exclude<NoteSegment, { kind: "text" }> };
  const hits: Hit[] = [];
  let at = 0;
  for (const s of linkify(text)) {
    if (s.kind === "link") hits.push({ start: at, end: at + s.text.length, seg: { kind: "link", text: s.text, href: s.href } });
    at += s.text.length;
  }
  const free = (a: number, b: number) => !hits.some((h) => a < h.end && b > h.start);
  for (const m of text.matchAll(EMAIL_RE)) {
    const start = m.index ?? 0;
    const end = start + m[0].length;
    if (free(start, end)) hits.push({ start, end, seg: { kind: "email", text: m[0], href: `mailto:${m[0]}` } });
  }
  for (const p of phoneSpans(text)) {
    if (free(p.start, p.end)) hits.push({ start: p.start, end: p.end, seg: { kind: "phone", text: p.text, href: p.href } });
  }
  for (const m of text.matchAll(ADDRESS_RE)) {
    const start = m.index ?? 0;
    const end = start + m[0].length;
    if (free(start, end)) hits.push({ start, end, seg: { kind: "address", text: m[0], query: m[0] } });
  }
  hits.sort((a, b) => a.start - b.start);
  const out: NoteSegment[] = [];
  let cursor = 0;
  for (const h of hits) {
    if (h.start < cursor) continue;
    if (h.start > cursor) out.push({ kind: "text", text: text.slice(cursor, h.start) });
    out.push(h.seg);
    cursor = h.end;
  }
  if (cursor < text.length) out.push({ kind: "text", text: text.slice(cursor) });
  return out;
}

/** Long notes start collapsed ("Show more"): over 280 characters or 5 lines. */
export const NOTES_COLLAPSE_CHARS = 280;
export const NOTES_COLLAPSE_LINES = 5;
export function notesCollapse(text: string): boolean {
  return text.length > NOTES_COLLAPSE_CHARS || text.split("\n").length > NOTES_COLLAPSE_LINES;
}

// ---------- the day strip ----------

export interface StripBlock {
  ev: CalEvent;
  /** 0-1 of the strip's height. */
  top: number;
  height: number;
  /** Side-by-side columns for overlapping events. */
  col: number;
  cols: number;
  self: boolean;
}

export interface DayStrip {
  fromMs: number;
  toMs: number;
  /** Whole hours shown as ticks (epoch ms). */
  hours: number[];
  blocks: StripBlock[];
}

const HOUR = 3_600_000;

function floorHour(ms: number): number {
  const d = new Date(ms);
  d.setMinutes(0, 0, 0);
  return d.getTime();
}

/**
 * The hours around a timed event (an hour either side, at least three) with the other timed events in them;
 * overlapping events share the width. Null for all-day events.
 */
export function dayStrip(ev: CalEvent, events: CalEvent[]): DayStrip | null {
  if (ev.allDay || !(ev.endMs >= ev.startMs)) return null;
  let fromMs = floorHour(ev.startMs - HOUR);
  let toMs = floorHour(ev.endMs + HOUR - 1) + HOUR;
  while (toMs - fromMs < 3 * HOUR) toMs += HOUR;
  // Very long events: cap at twelve hours from the hour before.
  toMs = Math.min(toMs, fromMs + 12 * HOUR);
  const inside = events.filter(
    (e) => !e.allDay && e.id !== ev.id && e.startMs < toMs && Math.max(e.endMs, e.startMs + 15 * 60_000) > fromMs,
  );
  const all = [...inside, ev].sort((a, b) => a.startMs - b.startMs || b.endMs - a.endMs || (a.id === ev.id ? -1 : 1));
  const span = toMs - fromMs;
  const blocks: StripBlock[] = [];
  // Greedy columns within clusters of overlapping events.
  let cluster: StripBlock[] = [];
  let colEnds: number[] = [];
  let clusterEnd = -Infinity;
  const flush = () => {
    for (const b of cluster) b.cols = colEnds.length;
    blocks.push(...cluster);
    cluster = [];
    colEnds = [];
  };
  for (const e of all) {
    const end = Math.max(e.endMs, e.startMs + 15 * 60_000);
    if (e.startMs >= clusterEnd) flush();
    let col = colEnds.findIndex((c) => c <= e.startMs);
    if (col < 0) {
      col = colEnds.length;
      colEnds.push(end);
    } else colEnds[col] = end;
    clusterEnd = Math.max(clusterEnd, end);
    const top = Math.max(0, (e.startMs - fromMs) / span);
    const bottom = Math.min(1, (end - fromMs) / span);
    cluster.push({ ev: e, top, height: Math.max(0.04, bottom - top), col, cols: 1, self: e.id === ev.id });
  }
  flush();
  const hours: number[] = [];
  for (let h = fromMs; h <= toMs; h += HOUR) hours.push(h);
  return { fromMs, toMs, hours, blocks };
}

// ---------- alerts, travel time, calendars ----------

/** Alert choices in minutes before the start (Apple's list). */
export const ALARM_CHOICES = [0, 5, 10, 15, 30, 60, 120, 1440, 2880, 10080] as const;
/** Travel time choices in minutes. */
export const TRAVEL_CHOICES = [5, 15, 30, 60, 90, 120] as const;
export const MAX_ALARMS = 5;

/** The parts of an alert's label: at the start, minutes, hours, days or weeks before. */
export function alarmParts(min: number): { at: true } | { unit: "min" | "h" | "d" | "w"; count: number } {
  if (min <= 0) return { at: true };
  if (min % 10080 === 0) return { unit: "w", count: min / 10080 };
  if (min % 1440 === 0) return { unit: "d", count: min / 1440 };
  if (min % 60 === 0) return { unit: "h", count: min / 60 };
  return { unit: "min", count: min };
}

/** The alerts after changing row `index` to `value` (null: removed; `index` past the end: added). */
export function setAlarm(alarms: number[], index: number, value: number | null): number[] {
  const next = [...alarms];
  if (value === null) next.splice(index, 1);
  else if (index >= next.length) next.push(value);
  else next[index] = value;
  return [...new Set(next)].sort((a, b) => a - b).slice(0, MAX_ALARMS);
}

/** When to leave: the start minus the travel time. */
export function leaveAtMs(e: Pick<CalEvent, "startMs" | "travelMin">): number | null {
  return e.travelMin && e.travelMin > 0 ? e.startMs - e.travelMin * 60_000 : null;
}

/** Whole minutes of a route, rounded up to five (what the travel-time suggestion shows). */
export function suggestedTravelMin(durationS: number): number {
  return Math.min(1440, Math.max(5, Math.ceil(durationS / 60 / 5) * 5));
}

export type CalendarTarget = "lucina" | "google";

/**
 * The calendars an event can be moved to (its own first): Lucina's and, when Google Calendar is connected, Google.
 * Empty when it can't move (read-only sources, repeating events).
 */
export function moveTargets(e: Pick<CalEvent, "source" | "editable" | "recurring">, googleConnected: boolean): CalendarTarget[] {
  if (!e.editable || e.recurring || (e.source !== "lucina" && e.source !== "google")) return [];
  if (!googleConnected) return [];
  return e.source === "lucina" ? ["lucina", "google"] : ["google", "lucina"];
}
