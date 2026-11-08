// Formatting and small rules for the Gallery viewer's info panel and dialogs (components/gallery/*).

import type { GalleryItem, Rotation } from "../api/gallery";

/** The Trash empties after this many days (the server's default `LUCINA_MEDIA_TRASH_DAYS`). */
export const TRASH_DAYS = 30;

const OFFSET = /^([+-])(\d{2}):?(\d{2})$/;

/** `+02:00` -> minutes east of UTC (120); null when it isn't an offset. */
export function offsetMinutes(offset: string | null | undefined): number | null {
  const m = OFFSET.exec((offset ?? "").trim());
  if (!m) return null;
  const minutes = Number(m[2]) * 60 + Number(m[3]);
  return m[1] === "-" ? -minutes : minutes;
}

/** `+02:00` -> "+02:00" (normalised), or null. */
export function normalizeOffset(offset: string | null | undefined): string | null {
  const minutes = offsetMinutes(offset);
  if (minutes === null) return null;
  const abs = Math.abs(minutes);
  return `${minutes < 0 ? "-" : "+"}${String(Math.floor(abs / 60)).padStart(2, "0")}:${String(abs % 60).padStart(2, "0")}`;
}

/**
 * The moment a picture was taken as the wall clock showed it where it was taken ("4 Mar 2026, 14:03") when the time
 * zone offset is known, else in this device's time zone. Empty for a missing or invalid date.
 */
export function formatTaken(takenAt: string | null | undefined, offset: string | null | undefined, lang: string): string {
  if (!takenAt) return "";
  const at = Date.parse(takenAt);
  if (Number.isNaN(at)) return "";
  const minutes = offsetMinutes(offset);
  const options: Intl.DateTimeFormatOptions = { weekday: "short", year: "numeric", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" };
  if (minutes === null) return new Intl.DateTimeFormat(lang, options).format(at);
  return new Intl.DateTimeFormat(lang, { ...options, timeZone: "UTC" }).format(at + minutes * 60_000);
}

/** 0.004 -> "1/250", 0.5 -> "1/2", 2 -> "2", 2.5 -> "2.5". */
export function formatExposure(seconds: number | null | undefined): string {
  if (seconds === null || seconds === undefined || !Number.isFinite(seconds) || seconds <= 0) return "";
  if (seconds >= 1) return String(Math.round(seconds * 10) / 10);
  return `1/${Math.round(1 / seconds)}`;
}

/** 2.8 -> "2.8", 4 -> "4". */
export function formatNumber(value: number | null | undefined, digits = 1): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return "";
  return String(Math.round(value * 10 ** digits) / 10 ** digits);
}

/** 12.2 million pixels -> "12.2". */
export function megapixels(width: number | null, height: number | null): string {
  if (!width || !height) return "";
  const mp = (width * height) / 1_000_000;
  return mp >= 1 ? String(Math.round(mp * 10) / 10) : "";
}

/** The next quarter turn clockwise. */
export function nextRotation(r: Rotation): Rotation {
  return ((r + 90) % 360) as Rotation;
}

/** The distinct device names behind these items (their id when the server has no name), in first-seen order. */
export function deviceNames(items: readonly GalleryItem[]): string[] {
  const seen = new Set<string>();
  for (const item of items) {
    if (!item.device) continue;
    seen.add(item.deviceName ?? item.device);
  }
  return [...seen];
}

/** "iPhone", "iPhone and Mac", "iPhone, Mac and iPad" in the current language. */
export function joinNames(names: readonly string[], lang: string): string {
  try {
    return new Intl.ListFormat(lang, { style: "long", type: "conjunction" }).format(names);
  } catch {
    return names.join(", ");
  }
}

/** Whole days left until `expiresAt` (never negative); null without a date. */
export function daysLeft(expiresAt: string | null | undefined, now = Date.now()): number | null {
  if (!expiresAt) return null;
  const at = Date.parse(expiresAt);
  if (Number.isNaN(at)) return null;
  return Math.max(0, Math.ceil((at - now) / 86_400_000));
}

/** The place's line: "Aarhus, Central Denmark, Denmark". */
export function placeLine(place: GalleryItem["place"]): string {
  if (!place) return "";
  const parts = [place.name, place.admin1, place.country].filter((p): p is string => !!p);
  return parts.filter((p, i) => parts.indexOf(p) === i).join(", ");
}
