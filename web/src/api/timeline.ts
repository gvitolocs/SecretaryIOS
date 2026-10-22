// The unified conversation timeline (GET /v1/timeline, GET /v1/timeline/stream).
//
// Every message Lucina sees or sends, from any channel, is one timeline item.
// Items from the network are untrusted: `parseItem` validates each one and
// drops anything malformed instead of throwing.

export const CHANNELS = ["telegram", "discord", "web", "ios", "alexa", "other"] as const;
export type Channel = (typeof CHANNELS)[number];

export interface Attachment {
  kind: "image" | "voice" | "file";
  name?: string;
}

export interface ItemMeta {
  chatTitle?: string;
  guildName?: string;
  channelName?: string;
  isDm?: boolean;
  kind?: "reminder" | "avatar_outfit";
  /** With kind "avatar_outfit": the outfit Lucina put on (absent: back to automatic). */
  outfit?: string;
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
  }
  return Object.keys(out).length ? out : undefined;
}

function parseAttachments(v: unknown): Attachment[] | undefined {
  if (!Array.isArray(v)) return undefined;
  const out: Attachment[] = [];
  for (const a of v) {
    if (!a || typeof a !== "object") continue;
    const kind = (a as { kind?: unknown }).kind;
    if (kind !== "image" && kind !== "voice" && kind !== "file") continue;
    const name = str((a as { name?: unknown }).name);
    out.push(name ? { kind, name } : { kind });
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
 */
export class TimelineStream {
  private es: EventSource | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private attempt = 0;
  private stopped = true;
  private needsResync = false;

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
        this.handlers.onStatus("reconnecting");
      }
    };
    es.addEventListener("item", (ev) => {
      if (this.es !== es) return;
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
