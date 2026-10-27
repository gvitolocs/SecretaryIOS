// The unified conversation timeline (GET /v1/timeline, GET /v1/timeline/stream).
//
// Every message Lucina sees or sends, from any channel, is one timeline item.
// Items from the network are untrusted: `parseItem` validates each one and
// drops anything malformed instead of throwing.

import type { PlaceInfo } from "../lib/mapKit";
import { parsePlace, parseRich, type RichMessage } from "../lib/rich";
import { type Model3dBlock, parseModel3dBlocks } from "../lib/model3d";

export const CHANNELS = ["telegram", "discord", "web", "ios", "alexa", "other"] as const;
export type Channel = (typeof CHANNELS)[number];

export interface Attachment {
  kind: "image" | "voice" | "file" | "video" | "audio" | "pdf" | "document" | "link" | "text" | "contact" | "location";
  name?: string;
  mime?: string;
  mediaId?: string;
  url?: string;
  bytes?: number;
  /** 0-based position in the original share's `file` parts. */
  index?: number;
  /** base64 ThumbHash placeholder, when lucina-media knows one. */
  thumbhash?: string;
  width?: number;
  height?: number;
  durationMs?: number;
  /** Location attachments. */
  lat?: number;
  lon?: number;
  /** This tab only: object URL of a just-picked file, before the server item arrives. */
  previewUrl?: string;
}

export interface ItemMeta {
  device?: string;
  deviceName?: string;
  via?: string;
  caller?: string;
  chatTitle?: string;
  guildName?: string;
  channelName?: string;
  isDm?: boolean;
  kind?: "reminder" | "avatar_outfit" | "note" | "notes" | "proactive" | "share";
  /** With kind "avatar_outfit": what Lucina put on (neither: back to automatic). */
  outfit?: string;
  hair?: string;
  /** With kind "note": the note Lucina saved or read. */
  slug?: string;
  /** Lucina message format v1 (meta.format "v1"): bubbles, widgets, buttons. */
  rich?: RichMessage;
  /** With kind "proactive": Lucina reached out on her own. */
  proactive?: ProactiveMeta;
  /** With kind "proactive": the place of the event it is about (calendar reminders). */
  place?: PlaceInfo;
  /** Message format v1 `model3d` blocks (3D models from the gallery). */
  blocks?: Model3dBlock[];
  /** With kind "share": the POST /v1/share idempotency key and the quick action. */
  shareId?: string;
  intent?: string;
}

export type ProactivePriority = "low" | "normal" | "high";

/** A button on a proactive message: a feedback tap, or a link into the app. */
export interface ProactiveAction {
  id: string;
  label: string;
  /** In-app path (content buttons). */
  route?: string;
  /** "useful" | "not_now" | "stop": posted as feedback. */
  feedback?: boolean;
}

export interface ProactiveMeta {
  /** `po_` + 26 base-32 characters; also in the deep link `/?outreach=<id>`. */
  outreachId: string;
  priority: ProactivePriority;
  /** What it is about (meeting_prep, service_down, pulse, ...). */
  topic?: string;
  /** Why Lucina reached out (shown on tap). */
  why?: string;
  actions: ProactiveAction[];
}

export const OUTREACH_ID = /^po_[0-9A-HJKMNP-TV-Z]{26}$/;
export const FEEDBACK_ACTIONS = ["useful", "not_now", "stop"] as const;
export type FeedbackAction = (typeof FEEDBACK_ACTIONS)[number];

/** An in-app path: one leading slash, no scheme or backslash, printable ASCII. */
export function safeRoute(v: unknown): string | undefined {
  if (typeof v !== "string" || v.length > 200) return undefined;
  if (!v.startsWith("/") || v.startsWith("//") || v.includes("\\")) return undefined;
  return /^[\x21-\x7e]+$/.test(v) ? v : undefined;
}

function parseProactive(m: Record<string, unknown>): ProactiveMeta | undefined {
  const id = m.outreachId;
  if (typeof id !== "string" || !OUTREACH_ID.test(id)) return undefined;
  const priority: ProactivePriority =
    m.priority === "high" || m.priority === "low" ? m.priority : "normal";
  const actions: ProactiveAction[] = [];
  if (Array.isArray(m.actions)) {
    for (const a of m.actions.slice(0, 8)) {
      if (!a || typeof a !== "object") continue;
      const r = a as Record<string, unknown>;
      const aid = typeof r.id === "string" && /^[a-z_]{1,32}$/.test(r.id) ? r.id : null;
      const label = str(r.label, 40);
      if (!aid || !label) continue;
      const feedback = r.feedback === true && (FEEDBACK_ACTIONS as readonly string[]).includes(aid);
      const route = feedback ? undefined : safeRoute(r.route);
      // A content button without a valid route does nothing: drop it.
      if (!feedback && !route) continue;
      actions.push(feedback ? { id: aid, label, feedback: true } : { id: aid, label, route: route! });
    }
  }
  const out: ProactiveMeta = { outreachId: id, priority, actions };
  const topic = typeof m.topic === "string" && /^[a-z_]{1,40}$/.test(m.topic) ? m.topic : undefined;
  if (topic) out.topic = topic;
  const why = str(m.why, 400);
  if (why) out.why = why;
  return out;
}

export interface TimelineItem {
  id: string;
  /** Milliseconds since the epoch. */
  ts: number;
  channel: Channel;
  role: "user" | "lucina";
  text: string;
  clientMessageId?: string;
  replyTo?: string;
  attachments?: Attachment[];
  meta?: ItemMeta;
  backfilled?: boolean;
}

export interface TimelinePage {
  items: unknown;
  cursor?: string | null;
  hasMore?: boolean;
  nextBefore?: string | number | null;
}

function idOf(v: unknown): string | null {
  if (typeof v === "string" && v) return v;
  if (typeof v === "number" && Number.isFinite(v)) return String(v);
  return null;
}

/** Accepts ISO strings, epoch milliseconds or epoch seconds. */
export function parseTs(v: unknown): number | null {
  if (typeof v === "number" && Number.isFinite(v)) return v < 1e12 ? v * 1000 : v;
  if (typeof v === "string" && v) {
    if (/^\d+(\.\d+)?$/.test(v)) return parseTs(Number(v));
    const t = Date.parse(v);
    return Number.isNaN(t) ? null : t;
  }
  return null;
}

function str(v: unknown, max = 200): string | undefined {
  return typeof v === "string" && v.trim() ? v.trim().slice(0, max) : undefined;
}

function parseMeta(v: unknown): ItemMeta | undefined {
  if (!v || typeof v !== "object") return undefined;
  const m = v as Record<string, unknown>;
  const out: ItemMeta = {};
  if (typeof m.device === "string") out.device = m.device;
  if (typeof m.deviceName === "string") out.deviceName = m.deviceName;
  if (typeof m.via === "string") out.via = m.via;
  if (typeof m.caller === "string") out.caller = m.caller;
  const chatTitle = str(m.chatTitle);
  const guildName = str(m.guildName);
  const channelName = str(m.channelName);
  if (chatTitle) out.chatTitle = chatTitle;
  if (guildName) out.guildName = guildName;
  if (channelName) out.channelName = channelName;
  if (typeof m.isDm === "boolean") out.isDm = m.isDm;
  if (m.kind === "reminder") out.kind = "reminder";
  if (m.kind === "avatar_outfit") {
    out.kind = "avatar_outfit";
    if (typeof m.outfit === "string" && /^[a-z0-9][a-z0-9_-]{0,39}$/.test(m.outfit)) out.outfit = m.outfit;
    if (typeof m.hair === "string" && /^[a-z0-9][a-z0-9_-]{0,39}$/.test(m.hair)) out.hair = m.hair;
  }
  if (m.kind === "notes") out.kind = "notes";
  const rich = parseRich(m);
  if (rich) out.rich = rich;
  if (m.kind === "proactive") {
    const p = parseProactive(m);
    if (p) {
      out.kind = "proactive";
      out.proactive = p;
      const place = parsePlace(m.place);
      if (place) out.place = place;
    }
  }
  if (m.kind === "note") {
    out.kind = "note";
    if (typeof m.slug === "string" && /^[a-z0-9][a-z0-9-]{0,63}$/.test(m.slug)) out.slug = m.slug;
  }
  if (m.kind === "share") {
    out.kind = "share";
    if (typeof m.shareId === "string" && /^[A-Za-z0-9-]{8,64}$/.test(m.shareId)) out.shareId = m.shareId;
    if (typeof m.intent === "string" && /^[a-z]{1,16}$/.test(m.intent)) out.intent = m.intent;
  }
  if (m.format === "v1") {
    const blocks = parseModel3dBlocks(m.blocks);
    if (blocks.length) out.blocks = blocks;
  }
  return Object.keys(out).length ? out : undefined;
}

function parseAttachments(v: unknown): Attachment[] | undefined {
  if (!Array.isArray(v)) return undefined;
  const out: Attachment[] = [];
  for (const a of v) {
    if (!a || typeof a !== "object") continue;
    const kind = (a as { kind?: unknown }).kind;
    if (typeof kind !== "string" || !["image", "voice", "file", "video", "audio", "pdf", "document", "link", "text", "contact", "location"].includes(kind)) continue;
    const raw = a as Record<string, unknown>;
    const attachment: Attachment = { kind: kind as Attachment["kind"] };
    for (const key of ["name", "mime", "mediaId", "url", "thumbhash"] as const) {
      const value = str(raw[key], key === "thumbhash" ? 400 : 200);
      if (value) attachment[key] = value;
    }
    if (typeof raw.bytes === "number" && Number.isFinite(raw.bytes) && raw.bytes >= 0) attachment.bytes = raw.bytes;
    if (typeof raw.index === "number" && Number.isInteger(raw.index) && raw.index >= 0) attachment.index = raw.index;
    for (const key of ["width", "height", "durationMs"] as const) {
      const value = raw[key];
      if (typeof value === "number" && Number.isFinite(value) && value >= 0) attachment[key] = value;
    }
    for (const key of ["lat", "lon"] as const) {
      const value = raw[key];
      if (typeof value === "number" && Number.isFinite(value)) attachment[key] = value;
    }
    out.push(attachment);
  }
  return out.length ? out : undefined;
}

export function parseItem(raw: unknown): TimelineItem | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const id = idOf(r.id);
  const ts = parseTs(r.ts);
  if (!id || ts === null) return null;
  if (r.role !== "user" && r.role !== "lucina") return null;
  const channel = (CHANNELS as readonly string[]).includes(r.channel as string) ? (r.channel as Channel) : "other";
  const item: TimelineItem = {
    id,
    ts,
    channel,
    role: r.role,
    text: typeof r.text === "string" ? r.text : "",
  };
  const cmid = idOf(r.clientMessageId);
  if (cmid) item.clientMessageId = cmid;
  const replyTo = idOf(r.replyTo);
  if (replyTo) item.replyTo = replyTo;
  const attachments = parseAttachments(r.attachments);
  if (attachments) item.attachments = attachments;
  const meta = parseMeta(r.meta);
  if (meta) item.meta = meta;
  if (r.backfilled === true) item.backfilled = true;
  return item;
}

/** Valid items of a page (newest first, as sent) and the cursor for older ones. */
export function parsePage(page: unknown): { items: TimelineItem[]; nextBefore: string | null } {
  const p = (page && typeof page === "object" ? page : {}) as TimelinePage;
  const items: TimelineItem[] = [];
  if (Array.isArray(p.items)) {
    for (const raw of p.items) {
      const it = parseItem(raw);
      if (it) items.push(it);
    }
  }
  return { items, nextBefore: idOf(p.nextBefore) };
}

// ---- Live stream -------------------------------------------------------------

export type StreamStatus = "connecting" | "live" | "reconnecting" | "offline";

export const STREAM_URL = "/v1/timeline/stream";
export const STREAM_BACKOFF_MS = [1_000, 2_000, 5_000, 10_000, 30_000] as const;

export interface StreamHandlers {
  onItem: (item: TimelineItem) => void;
  onStatus: (status: StreamStatus) => void;
  /** A brand-new connection opened after the previous one was closed for good: fill the gap. */
  onResync: () => void;
}

/**
 * Wraps EventSource. The browser reconnects on its own after a dropped
 * connection (sending Last-Event-ID, so the server replays the gap). When the
 * server answers with an error status instead, EventSource gives up
 * (readyState CLOSED); then this opens a new one with backoff and asks the
 * caller to resync, because a new EventSource has no Last-Event-ID.
 *
 * A connection that drops before it delivered any item reconnects without a
 * Last-Event-ID too (the browser only sends the id of an event it saw), so
 * the server starts that stream fresh and the items written in between would
 * be lost: resync on that reopen as well (docs/verification.md, bug T5).
 */
export class TimelineStream {
  private es: EventSource | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private attempt = 0;
  private stopped = true;
  private needsResync = false;
  /** The current EventSource has dispatched an item (so it has an id to resume from). */
  private sawItem = false;

  constructor(private readonly handlers: StreamHandlers) {}

  start(): void {
    if (!this.stopped) return;
    this.stopped = false;
    this.attempt = 0;
    this.needsResync = false;
    this.open();
  }

  stop(): void {
    this.stopped = true;
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    this.es?.close();
    this.es = null;
  }

  /** Reconnect right away (back online, tab visible again) unless already connected. */
  nudge(): void {
    if (this.stopped || (this.es && this.es.readyState === 1)) return;
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    this.es?.close();
    this.es = null;
    this.attempt = 0;
    this.open();
  }

  private open(): void {
    if (typeof EventSource === "undefined") {
      this.handlers.onStatus("offline");
      return;
    }
    this.handlers.onStatus(this.needsResync ? "reconnecting" : "connecting");
    let es: EventSource;
    try {
      es = new EventSource(STREAM_URL, { withCredentials: true });
    } catch {
      this.scheduleRetry();
      return;
    }
    this.es = es;
    this.sawItem = false;
    es.onopen = () => {
      if (this.es !== es) return;
      this.attempt = 0;
      this.handlers.onStatus("live");
      if (this.needsResync) {
        this.needsResync = false;
        this.handlers.onResync();
      }
    };
    es.onerror = () => {
      if (this.es !== es) return;
      if (es.readyState === 2) {
        es.close();
        this.es = null;
        this.scheduleRetry();
      } else {
        // The browser retries by itself; without a seen item it has no
        // Last-Event-ID to send, so fill the gap once it is back.
        if (!this.sawItem) this.needsResync = true;
        this.handlers.onStatus("reconnecting");
      }
    };
    es.addEventListener("item", (ev) => {
      if (this.es !== es) return;
      this.sawItem = true;
      let data: unknown;
      try {
        data = JSON.parse((ev as MessageEvent<string>).data);
      } catch {
        return;
      }
      const item = parseItem(data);
      if (item) this.handlers.onItem(item);
    });
  }

  private scheduleRetry(): void {
    if (this.stopped) return;
    this.needsResync = true;
    this.handlers.onStatus("reconnecting");
    const delay = STREAM_BACKOFF_MS[Math.min(this.attempt, STREAM_BACKOFF_MS.length - 1)]!;
    this.attempt++;
    this.timer = setTimeout(() => {
      this.timer = null;
      if (!this.stopped) this.open();
    }, delay);
  }
}
