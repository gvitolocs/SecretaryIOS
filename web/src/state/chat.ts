// The Chat timeline: one server-backed conversation across every channel.
//
// The server timeline is the source of truth. This module merges its pages
// (GET /v1/timeline), live items (SSE) and this tab's optimistic sends into one
// ordered list, deduped by server item id. A small sessionStorage cache of the
// last messages only speeds up the first paint.

import { createSignal } from "solid-js";
import { createStore, reconcile, unwrap } from "solid-js/store";
import { toApiError, type ApiError } from "../api/client";
import { api, CHAT_MAX_CHARS } from "../api/secretary";
import {
  CHANNELS,
  parsePage,
  TimelineStream,
  type Attachment,
  type Channel,
  type ItemMeta,
  type StreamStatus,
  type TimelineItem,
} from "../api/timeline";
import { chatSessionId, uuid } from "../lib/ids";
import { readJSON, removeKey, writeJSON } from "../lib/storage";
import { markLucinaOnline } from "./lucina";

export interface JobState {
  id: string;
  status: string;
  action: string | null;
  result?: unknown;
  /** Server-side failure reason when status is "error" (e.g. "interrupted"). */
  error?: string | null;
  /** Link to a generated image; opened as a link (CSP blocks remote <img>). */
  imageUrl?: string | null;
  /** Transient polling problem, shown under the card while it retries. */
  pollError: string | null;
}

export interface ChatMessage {
  /** Stable render identity; survives the optimistic -> server swap. */
  key: string;
  /** Server timeline item id, once known. */
  itemId?: string;
  role: "user" | "lucina";
  text: string;
  /** Milliseconds since the epoch (server time once confirmed). */
  createdAt: number;
  channel: Channel;
  clientMessageId?: string;
  /** Local Lucina replies: the clientMessageId of the message they answer. */
  replyFor?: string;
  replyTo?: string;
  attachments?: Attachment[];
  meta?: ItemMeta;
  backfilled?: boolean;
  /** This tab's own user messages only. */
  status?: "sending" | "sent" | "error";
  error?: string;
  action?: string | null;
  job?: JobState | null;
}

export type LoadState = "idle" | "loading" | "ready" | "error";

const CACHE_KEY = "secretary.timeline.v1";
const LEGACY_KEY = "secretary.chat.v1";
export const CACHE_LIMIT = 50;
export const JOB_POLL_MS = 3_000;
/** Refresh interval when the browser has no EventSource. */
export const FALLBACK_POLL_MS = 20_000;

export function isTerminalJob(status: string): boolean {
  return status === "done" || status === "error";
}

function hydrate(): ChatMessage[] {
  removeKey("session", LEGACY_KEY);
  const data = readJSON("session", CACHE_KEY);
  if (!Array.isArray(data)) return [];
  const out: ChatMessage[] = [];
  const seen = new Set<string>();
  for (const m of data) {
    if (!m || typeof m !== "object") continue;
    const msg = m as ChatMessage;
    if (typeof msg.key !== "string" || typeof msg.text !== "string" || seen.has(msg.key)) continue;
    if (msg.role !== "user" && msg.role !== "lucina") continue;
    if (typeof msg.createdAt !== "number" || !Number.isFinite(msg.createdAt)) continue;
    if (!(CHANNELS as readonly string[]).includes(msg.channel)) continue;
    seen.add(msg.key);
    // A reload interrupted this send; let the user retry it.
    out.push(msg.status === "sending" ? { ...msg, status: "error", error: "Not sent: the page was reloaded." } : msg);
  }
  return out.sort(byTime);
}

interface ChatStore {
  messages: ChatMessage[];
  /** Cursor for older items; null when the start of the timeline is loaded. */
  nextBefore: string | null;
  /** A server page has been merged in this tab. */
  loaded: boolean;
}

const [chat, setChat] = createStore<ChatStore>({ messages: hydrate(), nextBefore: null, loaded: false });
const [sending, setSending] = createSignal(false);
/** Latest Lucina reply, for the screen-reader live region. Not set on hydrate. */
const [lastReply, setLastReply] = createSignal("");
const [loadState, setLoadState] = createSignal<LoadState>("idle");
const [olderState, setOlderState] = createSignal<LoadState>("idle");
const [streamStatus, setStreamStatus] = createSignal<StreamStatus>("connecting");

export { chat, sending, lastReply, loadState, olderState, streamStatus };

const replyListeners = new Set<(text: string) => void>();

/** Called with the text of each reply to this tab's own messages (e.g. to speak it). */
export function onLucinaReply(cb: (text: string) => void): () => void {
  replyListeners.add(cb);
  return () => replyListeners.delete(cb);
}

/** More (older) items can be loaded from the server. */
export function hasMore(): boolean {
  return chat.loaded && chat.nextBefore !== null;
}

/** Bumped whenever the conversation is cleared, so late responses are dropped. */
let generation = 0;

function byTime(a: ChatMessage, b: ChatMessage): number {
  return a.createdAt - b.createdAt;
}

function persist(): void {
  writeJSON("session", CACHE_KEY, chat.messages.slice(-CACHE_LIMIT));
}

function find(key: string): ChatMessage | undefined {
  return chat.messages.find((m) => m.key === key);
}

/**
 * Structural change: edit a plain copy of the list, keep it time-ordered
 * (stable, so equal timestamps keep their arrival order) and reconcile it into
 * the store by `key`, so existing bubbles keep their DOM nodes.
 */
function commit(edit: (list: ChatMessage[]) => void): void {
  const list = chat.messages.map((m) => ({ ...unwrap(m) }));
  edit(list);
  list.sort(byTime);
  setChat("messages", reconcile(list, { key: "key", merge: false }));
  persist();
}

function serverFields(it: TimelineItem): Partial<ChatMessage> {
  return {
    itemId: it.id,
    role: it.role,
    text: it.text,
    createdAt: it.ts,
    channel: it.channel,
    replyTo: it.replyTo,
    attachments: it.attachments,
    meta: it.meta,
    backfilled: it.backfilled,
    ...(it.clientMessageId ? { clientMessageId: it.clientMessageId } : {}),
  };
}

function fromItem(it: TimelineItem): ChatMessage {
  return { key: `s:${it.id}`, ...serverFields(it) } as ChatMessage;
}

/** The existing message a server item stands for, if any. */
function matchItem(list: ChatMessage[], it: TimelineItem): ChatMessage | undefined {
  const byId = list.find((m) => m.itemId === it.id);
  if (byId) return byId;
  if (it.role === "user" && it.clientMessageId) {
    return list.find((m) => m.role === "user" && !m.itemId && m.clientMessageId === it.clientMessageId);
  }
  if (it.role === "lucina") {
    const cmid = it.clientMessageId ?? (it.replyTo ? list.find((m) => m.itemId === it.replyTo)?.clientMessageId : undefined);
    if (cmid) return list.find((m) => m.role === "lucina" && !m.itemId && m.replyFor === cmid);
  }
  return undefined;
}

/** Merge server items into `list`; returns how many were new. */
function mergeItems(list: ChatMessage[], items: TimelineItem[], where: "start" | "end"): number {
  const fresh: ChatMessage[] = [];
  for (const it of items) {
    const existing = matchItem(list, it) ?? fresh.find((m) => m.itemId === it.id);
    if (existing) Object.assign(existing, serverFields(it));
    else fresh.push(fromItem(it));
  }
  if (where === "start") list.unshift(...fresh);
  else list.push(...fresh);
  return fresh.length;
}

/** Give the message `key` the server id `itemId`, folding in a copy that arrived separately. */
function adopt(list: ChatMessage[], key: string, itemId: string): void {
  const mine = list.find((m) => m.key === key);
  if (!mine) return;
  const i = list.findIndex((m) => m.itemId === itemId && m.key !== key);
  if (i >= 0) {
    const dup = list[i]!;
    Object.assign(mine, {
      createdAt: dup.createdAt,
      channel: dup.channel,
      text: dup.text || mine.text,
      replyTo: dup.replyTo,
      attachments: dup.attachments,
      meta: dup.meta,
      backfilled: dup.backfilled,
    });
    list.splice(i, 1);
  }
  mine.itemId = itemId;
}

function idOrNull(v: unknown): string | null {
  if (typeof v === "string" && v) return v;
  if (typeof v === "number" && Number.isFinite(v)) return String(v);
  return null;
}

// ---- Loading -----------------------------------------------------------------

/** Fetch the newest page and merge it (first load, remount, resync after a stream gap). */
export async function loadLatest(): Promise<void> {
  if (loadState() === "loading") return;
  const gen = generation;
  setLoadState("loading");
  try {
    const page = parsePage(await api.getTimeline(null));
    if (gen !== generation) return;
    const first = !chat.loaded;
    let reset = false;
    commit((list) => {
      const known = new Set(list.map((m) => m.itemId).filter((id): id is string => !!id));
      const overlap = page.items.some((it) => known.has(it.id));
      if (known.size > 0 && !overlap && page.nextBefore !== null) {
        // Too much happened while away: what we hold no longer joins up with
        // the newest page. Keep only unconfirmed local messages.
        reset = true;
        for (let i = list.length - 1; i >= 0; i--) if (list[i]!.itemId) list.splice(i, 1);
      }
      mergeItems(list, [...page.items].reverse(), "end");
    });
    if (first || reset) setChat({ nextBefore: page.nextBefore, loaded: true });
    setLoadState("ready");
  } catch (e) {
    if (gen !== generation) return;
    // A 401 means signed out; the app returns to login.
    setLoadState(toApiError(e).status === 401 ? "idle" : "error");
  }
}

/** Fetch the page before the oldest loaded item. Returns how many new items were added. */
export async function loadOlder(): Promise<number> {
  const before = chat.nextBefore;
  if (!chat.loaded || before === null || olderState() === "loading") return 0;
  const gen = generation;
  setOlderState("loading");
  try {
    const page = parsePage(await api.getTimeline(before));
    if (gen !== generation) return 0;
    let added = 0;
    commit((list) => {
      added = mergeItems(list, [...page.items].reverse(), "start");
    });
    // A server that repeats the same cursor would loop forever; stop instead.
    setChat("nextBefore", page.nextBefore === before ? null : page.nextBefore);
    setOlderState("idle");
    return added;
  } catch {
    if (gen !== generation) return 0;
    setOlderState("error");
    return 0;
  }
}

/** Merge one live item (from the stream). */
export function receiveItem(item: TimelineItem): void {
  commit((list) => {
    mergeItems(list, [item], "end");
  });
}

// ---- Live stream ----------------------------------------------------------------

const stream = new TimelineStream({
  onItem: receiveItem,
  onStatus: setStreamStatus,
  onResync: () => void loadLatest(),
});

let connections = 0;
let connectionEpoch = 0;
let fallbackTimer: ReturnType<typeof setInterval> | null = null;

function onOnline(): void {
  stream.nudge();
}

function onVisible(): void {
  if (document.visibilityState !== "visible") return;
  if (streamStatus() !== "live") {
    stream.nudge();
    void loadLatest();
  }
}

function teardownStream(): void {
  stream.stop();
  if (fallbackTimer !== null) clearInterval(fallbackTimer);
  fallbackTimer = null;
  if (typeof window !== "undefined") {
    window.removeEventListener("online", onOnline);
    document.removeEventListener("visibilitychange", onVisible);
  }
}

/**
 * Load the newest items and keep the live stream open while a view needs it.
 * Returns the disconnect function (idempotent).
 */
export function connectTimeline(): () => void {
  connections++;
  const epoch = connectionEpoch;
  if (connections === 1) {
    void loadLatest();
    stream.start();
    if (typeof EventSource === "undefined") {
      fallbackTimer = setInterval(() => void loadLatest(), FALLBACK_POLL_MS);
    }
    window.addEventListener("online", onOnline);
    document.addEventListener("visibilitychange", onVisible);
  }
  let done = false;
  return () => {
    if (done || epoch !== connectionEpoch) return;
    done = true;
    connections--;
    if (connections === 0) teardownStream();
  };
}

// ---- Sending -------------------------------------------------------------------

export function describeChatError(err: ApiError): string {
  if (err.code === "lucina_unreachable") return "Lucina is offline. Not delivered.";
  if (err.code === "lucina_timeout") return "Lucina didn't answer in time.";
  if (err.code === "lucina_error") return "Lucina ran into a problem.";
  if (err.code === "timeout") return "No answer in time.";
  if (err.isNetwork) return "Couldn't reach the server.";
  if (err.status === 401) return "You were signed out.";
  if (err.status === 429) return "Too many messages; wait a moment.";
  return err.message || "Not delivered.";
}

async function deliver(key: string): Promise<void> {
  const msg = find(key);
  if (!msg || !msg.clientMessageId) return;
  const cmid = msg.clientMessageId;
  const text = msg.text;
  const gen = generation;
  setSending(true);
  try {
    const res = await api.sendChat({ message: text, sessionId: chatSessionId(), clientMessageId: cmid });
    markLucinaOnline();
    if (gen !== generation || !find(key)) return; // Cleared (e.g. logout) while in flight.
    const rawJob = res?.job && res.job.id !== undefined && res.job.id !== null ? res.job : null;
    const action = typeof res?.action === "string" ? res.action : null;
    const replyText = typeof res?.reply === "string" ? res.reply : "";
    const job: JobState | null = rawJob
      ? { id: String(rawJob.id), status: String(rawJob.status ?? "queued"), action, pollError: null }
      : null;
    const userItemId = idOrNull(res?.userItemId);
    const lucinaItemId = idOrNull(res?.lucinaItemId);
    let replyKey: string | null = null;

    commit((list) => {
      const mine = list.find((m) => m.key === key);
      if (mine) {
        mine.status = "sent";
        delete mine.error;
        if (userItemId) adopt(list, key, userItemId);
      }
      const userId = mine?.itemId;
      // Lucina's reply may already be here from the stream.
      const existing =
        (lucinaItemId ? list.find((m) => m.itemId === lucinaItemId) : undefined) ??
        list.find(
          (m) =>
            m.role === "lucina" &&
            !!m.itemId &&
            !m.replyFor &&
            (m.clientMessageId === cmid || (!!userId && m.replyTo === userId)),
        );
      if (existing) {
        existing.replyFor = cmid;
        existing.action = action;
        existing.job = job;
        if (!existing.text) existing.text = replyText;
        replyKey = existing.key;
        return;
      }
      if (!replyText && !job) return;
      const reply: ChatMessage = {
        key: `r:${cmid}`,
        role: "lucina",
        text: replyText,
        createdAt: Math.max(Date.now(), (mine?.createdAt ?? 0) + 1),
        channel: "web",
        replyFor: cmid,
        action,
        job,
      };
      if (lucinaItemId) reply.itemId = lucinaItemId;
      list.push(reply);
      replyKey = reply.key;
    });
    if (replyText) {
      setLastReply(replyText);
      for (const fn of replyListeners) fn(replyText);
    }
    if (job && replyKey) scheduleJobPoll(replyKey, isTerminalJob(job.status) ? 0 : JOB_POLL_MS);
  } catch (e) {
    const err = toApiError(e);
    if (gen !== generation || !find(key)) return;
    setChat("messages", (m) => m.key === key, { status: "error", error: describeChatError(err) });
    persist();
  } finally {
    if (gen === generation) setSending(false);
  }
}

/**
 * The server accepts 1-4000 characters with no control characters other than
 * newline and tab.
 */
export function cleanMessage(text: string): string {
  return text.replace(/\r\n?/g, "\n").replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/g, "").trim();
}

/** Adds an optimistic user bubble and sends it. Returns false if nothing was sent. */
export async function sendMessage(text: string): Promise<boolean> {
  const message = cleanMessage(text);
  if (message.length > CHAT_MAX_CHARS) return false;
  if (!message || sending()) return false;
  const cmid = uuid();
  const last = chat.messages[chat.messages.length - 1];
  const msg: ChatMessage = {
    key: `c:${cmid}`,
    role: "user",
    text: message,
    // Never sort above what is already on screen, even with a slow local clock.
    createdAt: Math.max(Date.now(), last ? last.createdAt + 1 : 0),
    channel: "web",
    clientMessageId: cmid,
    status: "sending",
  };
  commit((list) => {
    list.push(msg);
  });
  await deliver(msg.key);
  return true;
}

/** Resends a failed message with the same clientMessageId (idempotent on the server). */
export async function retryMessage(key: string): Promise<void> {
  const msg = find(key);
  if (!msg || msg.role !== "user" || msg.status !== "error" || sending()) return;
  setChat("messages", (m) => m.key === key, { status: "sending", error: undefined });
  persist();
  await deliver(key);
}

// ---- Job polling -----------------------------------------------------------

const timers = new Map<string, ReturnType<typeof setTimeout>>();

function scheduleJobPoll(key: string, delay = JOB_POLL_MS): void {
  const existing = timers.get(key);
  if (existing !== undefined) clearTimeout(existing);
  timers.set(
    key,
    setTimeout(() => {
      void pollJob(key);
    }, delay),
  );
}

function updateJob(key: string, patch: Partial<JobState>): void {
  setChat("messages", (m) => m.key === key && !!m.job, "job", (job) => (job ? { ...job, ...patch } : job));
  persist();
}

async function pollJob(key: string): Promise<void> {
  timers.delete(key);
  const jobId = find(key)?.job?.id;
  if (!jobId) return;
  try {
    const info = await api.getJob(jobId);
    if (!find(key)) return;
    const status = String(info?.status ?? "queued");
    const patch: Partial<JobState> = { status, pollError: null };
    if (typeof info?.action === "string") patch.action = info.action;
    if (info && "result" in info) patch.result = info.result;
    if (typeof info?.error === "string") patch.error = info.error;
    if (typeof info?.imageUrl === "string") patch.imageUrl = info.imageUrl;
    updateJob(key, patch);
    if (!isTerminalJob(status)) scheduleJobPoll(key);
  } catch (e) {
    const err = toApiError(e);
    if (!find(key)) return;
    if (err.status === 404 || err.code === "job_not_found") {
      updateJob(key, { status: "error", pollError: "This job no longer exists." });
      return;
    }
    if (err.status === 401) return; // Session ended; the app returns to login.
    updateJob(key, { pollError: "Couldn't check on this job. Retrying…" });
    scheduleJobPoll(key);
  }
}

/** Restart polling for unfinished jobs (after a reload or a remount). */
export function resumeJobPolling(): void {
  for (const m of chat.messages) {
    if (m.job && !isTerminalJob(m.job.status) && !timers.has(m.key)) scheduleJobPoll(m.key, 0);
  }
}

export function stopJobPolling(): void {
  for (const t of timers.values()) clearTimeout(t);
  timers.clear();
}

// ---- Reset -------------------------------------------------------------------

function reset(messages: ChatMessage[]): void {
  generation++;
  connectionEpoch++;
  connections = 0;
  teardownStream();
  stopJobPolling();
  setChat({ messages, nextBefore: null, loaded: false });
  setSending(false);
  setLastReply("");
  setLoadState("idle");
  setOlderState("idle");
  setStreamStatus("connecting");
}

/** Forget the conversation (logout). */
export function clearChat(): void {
  reset([]);
  removeKey("session", CACHE_KEY);
}

/** Test helper: reload state from the sessionStorage cache. */
export function reloadChatFromStorage(): void {
  reset(hydrate());
}
