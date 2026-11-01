// Lucina message format v1 (docs/lucina-message-format.md): short bubbles,
// typed widgets and buttons. Everything here comes from the network, so
// `parseRich` validates it again and drops what does not fit (never throws).

import { safeHref } from "./linkify";
import { isRfc3339, validLatLng, type PlaceInfo } from "./mapKit";

export type CardTone = "info" | "success" | "warning";

export type RichBlock =
  | { type: "checklist"; title?: string; items: { text: string; done?: boolean }[] }
  | { type: "card"; emoji?: string; title: string; subtitle?: string; body?: string; tone: CardTone }
  | { type: "stat"; items: { label: string; value: string; delta?: string }[] }
  | { type: "list"; title?: string; items: { emoji?: string; text: string; meta?: string }[] }
  | { type: "table"; title?: string; columns: string[]; rows: string[][] }
  | { type: "steps"; title?: string; items: string[] }
  | { type: "quote"; text: string; cite?: string }
  | { type: "link_preview"; url: string; title: string }
  | { type: "image"; src: string; alt: string; caption?: string; width?: number; height?: number }
  | ({ type: "place" } & PlaceInfo);

export type RichAction =
  | { id: string; label: string; emoji?: string; kind: "reply"; payload: { text: string; prefill?: boolean } }
  | { id: string; label: string; emoji?: string; kind: "route"; payload: { path: string } }
  | { id: string; label: string; emoji?: string; kind: "confirm"; payload: { answer: "yes" | "no" | "edit"; text?: string } }
  | { id: string; label: string; emoji?: string; kind: "dismiss"; payload: Record<string, never> }
  | { id: string; label: string; emoji?: string; kind: "secret"; payload: SecretActionPayload };

/** Opens the "Add secret" sheet. Names only: a value never travels in a message. */
export interface SecretActionPayload {
  mode: "save" | "project";
  name?: string;
  project?: string;
  environment?: string;
  path?: string;
  /** `/secret` target text (`poko`, `poko/prod/Pokobot`). */
  target?: string;
  /** A value Lucina holds in memory for the owner (the sheet sends the id). */
  held?: string;
  projectName?: string;
}

export interface RichMessage {
  format: "v1";
  lang?: "it" | "en" | "da";
  bubbles: string[];
  blocks: RichBlock[];
  actions: RichAction[];
}

export const MAX_BUBBLES = 4;
export const MAX_BLOCKS = 8;
export const MAX_ACTIONS = 4;
/** Lists show this many items before "show more". */
export const LIST_VISIBLE = 5;
export const MAX_TABLE_COLUMNS = 4;
export const MAX_TABLE_ROWS = 6;
/** Place blocks per message (they count against MAX_BLOCKS too). */
export const MAX_PLACES = 3;
/** Image blocks per message (they count against MAX_BLOCKS too). */
export const MAX_IMAGES = 2;

/** An image `src`: a path of this server's API (`/v1/…`) or https on getsecretary.tech; never `..` or `//`. */
const IMAGE_PATH = /^\/v1\/[A-Za-z0-9/_\-.%?=&~]{1,296}$/;

export function validImageSrc(src: string): boolean {
  if (!src || src.length > 300 || src.includes("..")) return false;
  if (IMAGE_PATH.test(src)) return !src.includes("//");
  const host = /^https:\/\/getsecretary\.tech(\/[A-Za-z0-9/_\-.%?=&~]*)?$/.exec(src);
  return !!host && !(host[1] ?? "").includes("//");
}

type Raw = Record<string, unknown>;

function obj(v: unknown): Raw | null {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Raw) : null;
}

function text(v: unknown, max: number): string {
  if (typeof v !== "string" && typeof v !== "number") return "";
  // Control characters other than newline and tab are dropped.
  const s = String(v)
    .replace(/\r\n?/g, "\n")
    .replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/g, "")
    .trim();
  return s.length > max ? `${s.slice(0, max - 1).trimEnd()}…` : s;
}

function line(v: unknown, max: number): string {
  return text(v, max).replace(/\s*\n\s*/g, " ");
}

function emoji(v: unknown): string | undefined {
  const s = line(v, 16);
  return s && !/[A-Za-z0-9]/.test(s) ? s : undefined;
}

function arr(v: unknown): unknown[] {
  return Array.isArray(v) ? v : [];
}

function withTitle<T extends object>(block: T, raw: Raw): T & { title?: string } {
  const title = line(raw.title, 120);
  return title ? { ...block, title } : block;
}

/**
 * A place (`place` block or a proactive item's `meta.place`): a title plus an
 * address or a valid coordinate pair, else null. A bad coordinate pair (one
 * missing, not finite, out of range, (0,0)) is dropped on its own; so are a
 * time that isn't RFC 3339 and an over-long event id.
 */
export function parsePlace(v: unknown): PlaceInfo | null {
  const b = obj(v);
  if (!b) return null;
  const title = line(b.title, 120);
  if (!title) return null;
  const place: PlaceInfo = { title, kind: b.kind === "event" ? "event" : "place" };
  const address = line(b.address, 200);
  if (address) place.address = address;
  if (validLatLng(b.lat, b.lng)) {
    place.lat = b.lat as number;
    place.lng = b.lng as number;
  }
  if (!address && place.lat === undefined) return null;
  for (const key of ["when", "end"] as const) {
    const t = b[key];
    if (typeof t === "string" && isRfc3339(t.trim())) place[key] = t.trim();
  }
  if (typeof b.eventId === "string" && b.eventId.trim() && b.eventId.length <= 200) place.eventId = b.eventId.trim();
  const mode = b.travelMode;
  if (Number.isInteger(b.travelMin) && (b.travelMin as number) >= 1 && (b.travelMin as number) <= 1440
    && (mode === "walk" || mode === "bike" || mode === "transit" || mode === "drive")) {
    place.travelMin = b.travelMin as number;
    place.travelMode = mode;
    if (b.travelApprox === true) place.travelApprox = true;
  }
  return place;
}

function parseBlock(v: unknown): RichBlock | null {
  const b = obj(v);
  if (!b) return null;
  switch (b.type) {
    case "checklist": {
      const items = arr(b.items)
        .map((i) => {
          const o = obj(i);
          const t = text(o ? o.text : i, 400);
          return t ? (o?.done === true ? { text: t, done: true } : { text: t }) : null;
        })
        .filter((i): i is { text: string; done?: boolean } => !!i)
        .slice(0, 12);
      return items.length ? withTitle({ type: "checklist" as const, items }, b) : null;
    }
    case "card": {
      const title = line(b.title, 120);
      const body = text(b.body, 600);
      if (!title && !body) return null;
      const tone: CardTone = b.tone === "success" || b.tone === "warning" ? b.tone : "info";
      const card: RichBlock & { type: "card" } = { type: "card", title: title || line(body, 60), tone };
      const e = emoji(b.emoji);
      if (e) card.emoji = e;
      const subtitle = line(b.subtitle, 120);
      if (subtitle) card.subtitle = subtitle;
      if (body && title) card.body = body;
      return card;
    }
    case "stat": {
      const source = Array.isArray(b.items) ? b.items : [b];
      const items = source
        .map((i) => {
          const o = obj(i);
          if (!o) return null;
          const label = line(o.label, 40);
          const value = line(o.value, 40);
          if (!label || !value) return null;
          const delta = line(o.delta, 40);
          return delta ? { label, value, delta } : { label, value };
        })
        .filter((i): i is { label: string; value: string; delta?: string } => !!i)
        .slice(0, 4);
      return items.length ? { type: "stat", items } : null;
    }
    case "list": {
      const items = arr(b.items)
        .map((i) => {
          const o = obj(i);
          const t = text(o ? o.text : i, 400);
          if (!t) return null;
          const item: { emoji?: string; text: string; meta?: string } = { text: t };
          const e = o ? emoji(o.emoji) : undefined;
          if (e) item.emoji = e;
          const meta = o ? line(o.meta, 80) : "";
          if (meta) item.meta = meta;
          return item;
        })
        .filter((i): i is { emoji?: string; text: string; meta?: string } => !!i)
        .slice(0, 20);
      return items.length ? withTitle({ type: "list" as const, items }, b) : null;
    }
    case "table": {
      const columns = arr(b.columns)
        .map((c) => line(c, 80))
        .slice(0, MAX_TABLE_COLUMNS);
      if (!columns.length || columns.every((c) => !c)) return null;
      const rows = arr(b.rows)
        .filter(Array.isArray)
        .map((r) => columns.map((_, k) => line((r as unknown[])[k], 80)))
        .filter((r) => r.some(Boolean))
        .slice(0, MAX_TABLE_ROWS);
      return rows.length ? withTitle({ type: "table" as const, columns, rows }, b) : null;
    }
    case "steps": {
      const items = arr(b.items)
        .map((i) => text(obj(i) ? obj(i)!.text : i, 400))
        .filter(Boolean)
        .slice(0, 8);
      return items.length ? withTitle({ type: "steps" as const, items }, b) : null;
    }
    case "quote": {
      const t = text(b.text, 600);
      if (!t) return null;
      const cite = line(b.cite, 120);
      return cite ? { type: "quote", text: t, cite } : { type: "quote", text: t };
    }
    case "link_preview": {
      const raw = line(b.url, 2000);
      const url = /^https?:\/\//i.test(raw) ? safeHref(raw) : null;
      if (!url) return null;
      return { type: "link_preview", url, title: line(b.title, 120) || new URL(url).hostname };
    }
    case "place": {
      const place = parsePlace(b);
      return place ? { type: "place", ...place } : null;
    }
    case "image": {
      const src = typeof b.src === "string" ? b.src.trim() : "";
      if (!validImageSrc(src)) return null;
      const caption = line(b.caption, 200);
      const image: RichBlock & { type: "image" } = { type: "image", src, alt: line(b.alt, 200) || caption };
      if (caption) image.caption = caption;
      for (const key of ["width", "height"] as const) {
        const n = b[key];
        if (Number.isInteger(n) && (n as number) > 0 && (n as number) <= 4096) image[key] = n as number;
      }
      return image;
    }
    default:
      return null;
  }
}

const ROUTE = /^\/[A-Za-z0-9/_\-.%]{0,120}$/;
/** The in-app place map with its percent-encoded query (Lucina's "Naviga"), at most 600 characters. */
const MAP_ROUTE = /^\/map\/place\?[A-Za-z0-9%=&_.~-]{1,589}$/;
const ACTION_ID = /^[A-Za-z0-9_.:-]{1,48}$/;

function parseAction(v: unknown, index: number): RichAction | null {
  const a = obj(v);
  if (!a) return null;
  const label = line(a.label, 40);
  if (!label) return null;
  const id = typeof a.id === "string" && ACTION_ID.test(a.id) ? a.id : `a${index + 1}`;
  const p = obj(a.payload) ?? {};
  const base = { id, label, ...(emoji(a.emoji) ? { emoji: emoji(a.emoji)! } : {}) };
  switch (a.kind) {
    case "reply": {
      const t = text(p.text, 500);
      if (!t) return null;
      return { ...base, kind: "reply", payload: p.prefill === true ? { text: t, prefill: true } : { text: t } };
    }
    case "route": {
      const path = line(p.path, 601);
      const app = ROUTE.test(path) && !path.includes("..") && !path.startsWith("//");
      if (!app && !MAP_ROUTE.test(path)) return null;
      return { ...base, kind: "route", payload: { path } };
    }
    case "confirm": {
      const answer = p.answer;
      if (answer !== "yes" && answer !== "no" && answer !== "edit") return null;
      const t = text(p.text, 500);
      if (answer !== "edit" && !t) return null;
      return { ...base, kind: "confirm", payload: t ? { answer, text: t } : { answer } };
    }
    case "dismiss":
      return { ...base, kind: "dismiss", payload: {} };
    case "secret": {
      if (p.mode !== "save" && p.mode !== "project") return null;
      const payload: SecretActionPayload = { mode: p.mode };
      const NAME = /^[A-Za-z0-9_.@/-]{1,256}$/;
      for (const k of ["name", "project", "environment", "path", "target", "held"] as const) {
        const v = p[k];
        if (typeof v === "string" && NAME.test(v)) payload[k] = v;
      }
      const projectName = line(p.projectName, 64);
      if (projectName) payload.projectName = projectName;
      return { ...base, kind: "secret", payload };
    }
    default:
      return null;
  }
}

/** A v1 message from a chat response or timeline meta, or null. */
export function parseRich(raw: unknown): RichMessage | null {
  const m = obj(raw);
  if (!m || m.format !== "v1") return null;
  const bubbles = arr(m.bubbles)
    .map((b) => text(b, 900))
    .filter(Boolean)
    .slice(0, MAX_BUBBLES);
  if (!bubbles.length) return null;
  let places = 0;
  let images = 0;
  const blocks = arr(m.blocks)
    .map(parseBlock)
    .filter(
      (b): b is RichBlock =>
        !!b && (b.type !== "place" || ++places <= MAX_PLACES) && (b.type !== "image" || ++images <= MAX_IMAGES),
    )
    .slice(0, MAX_BLOCKS);
  const actions: RichAction[] = [];
  const ids = new Set<string>();
  for (const raw of arr(m.actions)) {
    const a = parseAction(raw, actions.length);
    if (!a || ids.has(a.id)) continue;
    ids.add(a.id);
    actions.push(a);
    if (actions.length >= MAX_ACTIONS) break;
  }
  const lang = m.lang === "it" || m.lang === "en" || m.lang === "da" ? m.lang : undefined;
  return { format: "v1", ...(lang ? { lang } : {}), bubbles, blocks, actions };
}

/** Inline markdown marks removed (speech, the avatar's speech bubble). */
export function stripMarkdown(s: string): string {
  return s
    .replace(/!?\[([^\]]*)\]\(([^)\s]+)\)/g, "$1")
    .replace(/\*\*([^*\n]+)\*\*/g, "$1")
    .replace(/__([^_\n]+)__/g, "$1")
    .replace(/(^|[\s(])\*([^*\n]+)\*(?=$|[\s).,;:!?])/g, "$1$2")
    .replace(/(^|[\s(])_([^_\n]+)_(?=$|[\s).,;:!?])/g, "$1$2")
    .replace(/~~([^~\n]+)~~/g, "$1")
    .replace(/`([^`\n]+)`/g, "$1")
    .replace(/^#{1,6}\s+/gm, "")
    .replace(/^\s*[-*+•]\s+/gm, "");
}

/** What her voice reads: the bubbles only. */
export function speechText(m: RichMessage): string {
  return stripMarkdown(m.bubbles.join(" ")).replace(/\s+/g, " ").trim();
}

/** The text a button sends (or prefills), if any. */
export function actionText(a: RichAction): string | null {
  if (a.kind === "reply") return a.payload.text;
  if (a.kind === "confirm") return a.payload.text ?? null;
  return null;
}

/** Typing pause before bubble `i` (ms): longer bubbles take a little longer. */
export function typingDelay(bubble: string): number {
  return Math.min(900, 350 + Math.round(bubble.length * 2.2));
}
