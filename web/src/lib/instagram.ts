import type { IgItem, IgKind } from "../api/instagram";

// Pure helpers for the Instagram view: what the owner typed, the viewer's slides, counts.

export type Typed = { kind: "user"; username: string } | { kind: "post"; url: string; username: string | null };

const USERNAME = /^[a-z0-9._]{1,30}$/i;
const RESERVED = new Set(["p", "reel", "reels", "tv", "stories", "explore", "accounts", "direct"]);

/** "natgeo", "@natgeo", instagram.com/natgeo/, or an instagram.com/p/… or /reel/… link. */
export function parseTyped(raw: string): Typed | null {
  const text = raw.trim();
  if (!text) return null;
  if (/^(https?:\/\/)?([a-z0-9-]+\.)?(instagram\.com|instagr\.am)(\/|$)/i.test(text)) {
    let url: URL;
    try {
      url = new URL(/^https?:\/\//i.test(text) ? text : `https://${text}`);
    } catch {
      return null;
    }
    const segments = url.pathname.split("/").filter(Boolean);
    const at = segments.findIndex((s) => ["p", "reel", "reels", "tv"].includes(s));
    if (at >= 0 && segments[at + 1]) {
      // instagram.com/<user>/p/<code>/ names the owner; instagram.com/p/<code>/ doesn't.
      const owner = at > 0 ? segments[0]!.toLowerCase() : null;
      return { kind: "post", url: url.toString(), username: owner && USERNAME.test(owner) ? owner : null };
    }
    const first = segments[0]?.toLowerCase();
    if (first && !RESERVED.has(first) && USERNAME.test(first)) return { kind: "user", username: first };
    return null;
  }
  const name = text.replace(/^@/, "");
  return USERNAME.test(name) ? { kind: "user", username: name.toLowerCase() } : null;
}

export interface Slide {
  key: string;
  kind: IgKind;
  url: string;
  thumbUrl: string | null;
  item: IgItem;
  /** Position inside a carousel (0 for a single item) and the carousel's length. */
  frame: number;
  frames: number;
}

/** The viewer's slides: a carousel becomes one slide per frame. */
export function slidesOf(items: IgItem[]): Slide[] {
  const out: Slide[] = [];
  for (const item of items) {
    if (item.kind === "carousel") {
      const kids = item.children ?? [];
      if (kids.length === 0) {
        out.push({ key: item.id, kind: "image", url: item.url, thumbUrl: item.thumbUrl, item, frame: 0, frames: 1 });
        continue;
      }
      kids.forEach((c, i) =>
        out.push({ key: c.id, kind: c.kind, url: c.url, thumbUrl: c.thumbUrl, item, frame: i, frames: kids.length }),
      );
    } else {
      out.push({ key: item.id, kind: item.kind, url: item.url, thumbUrl: item.thumbUrl, item, frame: 0, frames: 1 });
    }
  }
  return out;
}

/** The index of the first slide of `items[index]`. */
export function slideIndexOf(items: IgItem[], index: number): number {
  return slidesOf(items.slice(0, index)).length;
}

/** 1 234 -> "1,234"; 12 500 -> "12.5K"; 268 000 000 -> "268M". */
export function formatCount(n: number): string {
  if (!Number.isFinite(n)) return "0";
  const abs = Math.abs(n);
  const short = (v: number, unit: string) => `${v >= 100 ? Math.round(v) : Math.round(v * 10) / 10}${unit}`;
  if (abs >= 1_000_000_000) return short(n / 1_000_000_000, "B");
  if (abs >= 1_000_000) return short(n / 1_000_000, "M");
  if (abs >= 10_000) return short(n / 1_000, "K");
  return String(Math.round(n));
}

/** "2 h ago"-free: a plain local date for a Unix-seconds timestamp. */
export function formatTaken(seconds: number | null | undefined, locale?: string): string {
  if (!seconds) return "";
  try {
    return new Date(seconds * 1000).toLocaleDateString(locale, { year: "numeric", month: "short", day: "numeric" });
  } catch {
    return "";
  }
}

/**
 * When a watched account is checked next, as a clock time: "today 15:40", "tomorrow 09:05", "Sat 15:40". A countdown
 * ("in 7 seconds") goes stale on screen and read as if the account were checked all the time. `null` for a check that
 * is due now (the first one after Watch, or one that is late): the caller says "in a moment".
 */
export function nextCheckText(seconds: number, nowMs = Date.now(), lang?: string): string | null {
  const at = new Date(seconds * 1000);
  if (at.getTime() - nowMs < 60_000) return null;
  const now = new Date(nowMs);
  const day = (d: Date) => Math.floor(new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime() / 86_400_000);
  const days = Math.round(day(at) - day(now));
  const time = at.toLocaleTimeString(lang, { hour: "2-digit", minute: "2-digit" });
  if (days <= 1) return `${new Intl.RelativeTimeFormat(lang, { numeric: "auto" }).format(days, "day")} ${time}`;
  return `${at.toLocaleDateString(lang, { weekday: "short" })} ${time}`;
}
