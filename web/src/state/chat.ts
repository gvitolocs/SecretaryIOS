// The Chat tab: this device's main conversation with Lucina.
//
// The server timeline is the source of truth; this device's part of it ("mine": its own
// device conversation, items from before devices were recorded, and Lucina's own
// messages) is kept in IndexedDB (lib/chatdb). The first paint comes from there, then
// `GET /v1/timeline?scope=mine&after=<cursor>` fetches only what arrived since
// (lib/chatSync). Live items (SSE) are all stored, and only "mine" are shown here; every
// other conversation is in the All chats plugin (views/Chats). This tab's optimistic
// sends are merged in and deduped by server item id.

import { createSignal } from "solid-js";
import { createStore, reconcile, unwrap } from "solid-js/store";
import { toApiError, type ApiError } from "../api/client";
import { cardsApi, type ServerCard } from "../api/cards";
import { api, CHAT_MAX_CHARS } from "../api/secretary";
import { refreshSecretQueue } from "./secretRequests";
import {
  TimelineStream,
  type Attachment,
  type Channel,
  type ItemMeta,
  type StreamStatus,
  type TimelineItem,
} from "../api/timeline";
import { t } from "../i18n";
import {
  buildShareMeta,
  draftAttachment,
  locationAttachment,
  releaseDraft,
  type DraftAttachment,
  type ShareLocation,
} from "../lib/attachments";
import {
  newShareId,
  pollShareStatus,
  postChatShare,
  type ShareStage,
  type ShareStatus,
  type ShareStatusAttachment,
} from "../lib/chatShare";
import { chatSessionId, uuid } from "../lib/ids";
import { parseRich, speechText, stripMarkdown, type RichMessage } from "../lib/rich";
import { isSecretCommand } from "../lib/secrets";
import { readJSON, removeKey, writeJSON } from "../lib/storage";
import { clearChatDB, getState, setState, isMine, putItems, readItems } from "../lib/chatdb";
import { olderScope, syncScope } from "../lib/chatSync";
import { markSynced } from "../lib/chatPerf";
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

/** A chat composer attachment send: upload progress, then the reading stage. */
export interface ChatShare {
  /** The idempotency key also used by POST /v1/share (retry keeps it). */
  id: string;
  files: DraftAttachment[];
  progress: number;
  phase: "uploading" | ShareStage;
  /** The optional `meta` JSON built when the send started. */
  meta?: string;
  error?: string;
  stage?: ShareStage;
  attachments?: ShareStatusAttachment[];
}

export interface ChatMessage {
  sensitive?: boolean;
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
  /** Lucina message format v1 from the chat response (timeline items carry it in meta.rich). */
  rich?: RichMessage;
  /** Arrived while the chat was open: reveal its bubbles one by one. Never cached. */
  reveal?: boolean;
  /** This tab's attachment upload/reading state. */
  share?: ChatShare;
}

/** The v1 rich message of a chat message, if it has one. */
export function richOf(m: ChatMessage): RichMessage | undefined {
  return m.meta?.rich ?? m.rich;
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
  // The sessionStorage caches are gone (IndexedDB holds the chat); clean up old copies.
  removeKey("session", CACHE_KEY);
  removeKey("session", LEGACY_KEY);
  return [];
}
let deviceId: string | undefined;
export function setChatDevice(id?: string): void { deviceId = id; void setState("deviceId", id ?? null); }
export function chatDevice(): string | undefined { return deviceId; }
export let firstMessageSource: "cache" | "server" = "server";
const [replyTarget, setReplyTarget] = createSignal<{ id: string; text: string } | null>(null);
export { replyTarget, setReplyTarget };

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

// ---- Rich replies: composer prefill, reveal progress, chosen buttons ----------

/** Text a button wants in the composer (`prefill` replies, "Modifica"); `n` makes repeats distinct. */
const [prefill, setPrefill] = createSignal<{ text: string; n: number } | null>(null);
let prefillCount = 0;
export { prefill };
export function requestPrefill(text: string): void {
  setPrefill({ text, n: ++prefillCount });
}

/** Bumped whenever a revealing reply shows another bubble (keeps the list pinned to the end). */
const [revealTick, setRevealTick] = createSignal(0);
export { revealTick };
export function bumpReveal(): void {
  setRevealTick((n) => n + 1);
}

const CHOICES_KEY = "secretary.chat.choices.v1";
const CHOICES_LIMIT = 200;

function readChoices(): Record<string, string> {
  const data = readJSON("local", CHOICES_KEY);
  if (!data || typeof data !== "object" || Array.isArray(data)) return {};
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(data as Record<string, unknown>)) if (typeof v === "string") out[k] = v;
  return out;
}

const [choices, setChoices] = createStore<Record<string, string>>(readChoices());
// What the server says about each card; it outranks this device's own memory.
const [serverCards, setServerCards] = createStore<Record<string, ServerCard>>({});

/** The button already used on this message (by item id once known, else its key). */
export function chosenAction(m: ChatMessage): string | undefined {
  return (m.itemId && (serverCards[m.itemId]?.action ?? choices[m.itemId])) || choices[m.key];
}

/** The server closed this card (decided, expired, withdrawn, or a button used on another client). */
export function cardClosed(m: ChatMessage): boolean {
  return !!m.itemId && serverCards[m.itemId]?.closed === true;
}

export function chooseAction(m: ChatMessage, actionId: string): void {
  const id = m.itemId ?? m.key;
  setChoices(id, actionId);
  const all = Object.entries(unwrap(choices));
  writeJSON("local", CHOICES_KEY, Object.fromEntries(all.slice(-CHOICES_LIMIT)));
  // The other clients hear about it; if this fails only this device remembers.
  if (m.itemId) cardsApi.choose(m.itemId, actionId).catch(() => {});
}

export async function refreshCards(): Promise<void> {
  try {
    setServerCards(reconcile(await cardsApi.list()));
  } catch {
    // Offline or an older server: buttons fall back to this device's memory.
  }
}

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
const sentClientMessageIds = new Set<string>();

function byTime(a: ChatMessage, b: ChatMessage): number {
  return a.createdAt - b.createdAt;
}

function persist(): void {
  const items: TimelineItem[] = chat.messages.map(m => unwrap(m)).filter(m => m.itemId && !m.sensitive && !m.key.startsWith("s:")).map(m => ({
    id: m.itemId!, ts: m.createdAt, channel: m.channel, role: m.role, text: m.text,
    clientMessageId: m.clientMessageId, replyTo: m.replyTo, backfilled: m.backfilled,
    meta: { ...m.meta, ...(m.rich ? { rich: m.rich } : {}) },
    attachments: m.attachments?.map(({ previewUrl: _preview, ...a }) => a),
  }));
  if (items.length) void putItems(items);
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
  const fields: Partial<ChatMessage> = {
    itemId: it.id,
    role: it.role,
    text: it.text,
    createdAt: it.ts,
    channel: it.channel,
    replyTo: it.replyTo,
    meta: it.meta,
    backfilled: it.backfilled,
    ...(it.clientMessageId ? { clientMessageId: it.clientMessageId } : {}),
  };
  // Keep the optimistic previews when the server item has no attachment list.
  if (it.attachments) fields.attachments = it.attachments;
  return fields;
}

export function fromItem(it: TimelineItem): ChatMessage {
  return { key: `s:${it.id}`, ...serverFields(it) } as ChatMessage;
}

/** The existing message a server item stands for, if any. */
function matchItem(list: ChatMessage[], it: TimelineItem): ChatMessage | undefined {
  const byId = list.find((m) => m.itemId === it.id);
  if (byId) return byId;
  if (it.role === "user" && it.clientMessageId) {
    return list.find((m) => m.role === "user" && !m.itemId && m.clientMessageId === it.clientMessageId);
  }
  // A composer share's server item can arrive (stream, poll) before the 202, or
  // without it when the 202 is lost: match it to the local bubble by shareId
  // (Stateright SH5, Verus SK2.4; iOS Timeline.merge does the same).
  const shareId = it.role === "user" ? it.meta?.shareId : undefined;
  if (shareId) {
    return list.find((m) => m.role === "user" && !m.itemId && m.share?.id === shareId);
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

/** Read local history before session/network work finishes on a returning visit. */
export async function hydrateCachedChat(): Promise<void> {
  const gen = generation;
  const savedDevice = await getState<string>("deviceId");
  if (gen !== generation) return;
  deviceId ??= savedDevice;
  const cached = await readItems("mine", deviceId);
  if (gen !== generation || chat.messages.length || !cached.length) return;
  firstMessageSource = "cache";
  commit(list => { mergeItems(list, [...cached].reverse(), "end"); });
  setChat({ nextBefore: cached[cached.length - 1]!.id, loaded: true });
}

/** Fetch the newest page and merge it (first load, remount, resync after a stream gap). */
export async function loadLatest(): Promise<void> {
  if (loadState() === "loading") return;
  const gen = generation;
  setLoadState("loading");
  try {
    const first = !chat.loaded;
    const cached = await readItems("mine", deviceId);
    if (gen !== generation) return;
    if (!chat.messages.length && cached.length) {
      firstMessageSource = "cache";
      commit(list => { mergeItems(list, [...cached].reverse(), "end"); });
      setChat({ nextBefore: cached[cached.length - 1]!.id, loaded: true });
      // Let Solid paint cached messages before starting the request.
      await new Promise<void>(resolve => setTimeout(resolve, 0));
    }
    const page = await syncScope("mine", deviceId, () => gen === generation);
    if (gen !== generation) return;
    commit(list => {
      if (page.reset) for (let i = list.length - 1; i >= 0; i--) if (list[i]!.itemId) list.splice(i, 1);
      mergeItems(list, [...page.items].reverse(), "end");
    });
    if (first || page.reset) setChat({ nextBefore: page.nextBefore, loaded: true });
    markSynced();
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
    const oldest = chat.messages.find(m => m.itemId)?.itemId ?? before;
    const cached = await readItems("mine", deviceId, oldest);
    const page = cached.length ? { items: cached, nextBefore: cached[cached.length - 1]!.id } : await olderScope("mine", before);
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

const itemListeners = new Set<(item: TimelineItem) => void>();

/** Called with each live item from the stream (not with pages), e.g. Lucina's outfit changes. */
export function onTimelineItem(cb: (item: TimelineItem) => void): () => void {
  itemListeners.add(cb);
  return () => itemListeners.delete(cb);
}

/** Merge one live item (from the stream). */
export function receiveItem(item: TimelineItem): void {
  void putItems([item]);
  for (const cb of itemListeners) cb(item);
  if (!isMine(item, deviceId) && !(item.clientMessageId && sentClientMessageIds.has(item.clientMessageId))) return;
  commit((list) => {
    const added = mergeItems(list, [item], "end");
    // A rich reply that arrives live is "said" bubble by bubble.
    if (added && item.role === "lucina" && item.meta?.rich) {
      const m = list.find((x) => x.itemId === item.id);
      if (m) m.reveal = true;
    }
    // Lucina answered an attachment: no more "reading…" state.
    if (item.role === "lucina") finishShareForReply(list, item.replyTo);
  });
}

function finishShareForReply(list: ChatMessage[], replyTo: string | undefined): void {
  if (!replyTo) return;
  for (const m of list) {
    if (!m.share || m.itemId !== replyTo) continue;
    if (m.share.phase !== "done") {
      // A new object: commit's copy is shallow, so nested edits must not mutate the store.
      m.share = { ...m.share, phase: "done", stage: "done" };
      stopSharePoll(m.share.id);
    }
  }
}

// ---- Live stream ----------------------------------------------------------------

const connectedListeners = new Set<() => void>();
export function onTimelineConnected(cb: () => void): () => void {
  connectedListeners.add(cb); return () => { connectedListeners.delete(cb); };
}
const stream = new TimelineStream({
  onItem: receiveItem,
  onStatus: status => { setStreamStatus(status); if (status === "live") { void loadLatest(); void refreshCards(); for (const cb of connectedListeners) cb(); } },
  onResync: () => void loadLatest(),
  onCards: () => {
    void refreshCards();
    void refreshSecretQueue();
  },
});

let connections = 0;
let connectionEpoch = 0;
let fallbackTimer: ReturnType<typeof setInterval> | null = null;

function onOnline(): void {
  stream.nudge();
}

function onVisible(): void {
  if (document.visibilityState !== "visible") return;
  stream.nudge();
  void loadLatest();
  void refreshCards();
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
    void refreshCards();
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
  if (err.code === "lucina_unreachable") return t("chat.errOffline");
  if (err.code === "lucina_timeout") return t("chat.errTimeout");
  if (err.code === "lucina_error") return t("chat.errProblem");
  if (err.code === "timeout") return t("chat.errNoAnswer");
  if (err.isNetwork) return t("chat.errNetwork");
  if (err.status === 401) return t("chat.errSignedOut");
  if (err.status === 429) return t("chat.errTooMany");
  return err.message || t("chat.notDelivered");
}

async function deliver(key: string): Promise<void> {
  const msg = find(key);
  if (!msg || !msg.clientMessageId) return;
  const cmid = msg.clientMessageId;
  sentClientMessageIds.add(cmid);
  const text = msg.text;
  const gen = generation;
  setSending(true);
  try {
    const res = await api.sendChat({ message: text, sessionId: chatSessionId(), clientMessageId: cmid, ...(msg.replyTo ? { replyTo: msg.replyTo } : {}) });
    markLucinaOnline();
    if (gen !== generation || !find(key)) return; // Cleared (e.g. logout) while in flight.
    const rawJob = res?.job && res.job.id !== undefined && res.job.id !== null ? res.job : null;
    const action = typeof res?.action === "string" ? res.action : null;
    const replyText = typeof res?.reply === "string" ? res.reply : "";
    const sensitive = res?.sensitive === true;
    const rich = sensitive ? undefined : parseRich(res) ?? undefined;
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
        if (rich && !existing.meta?.rich) existing.rich = rich;
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
        ...(sensitive ? { sensitive: true } : {}),
        ...(rich ? { rich, reveal: true } : {}),
      };
      if (lucinaItemId) reply.itemId = lucinaItemId;
      list.push(reply);
      replyKey = reply.key;
    });
    if (sensitive && replyKey && res.expiresAt) {
      const expiringKey = replyKey;
      const timer = setTimeout(() => {
        if (gen !== generation) return;
        commit((list) => { const m = list.find((v) => v.key === expiringKey); if (m) m.text = "OTP expired."; });
        setLastReply("");
      }, Math.max(0, Math.min(120_000, res.expiresAt - Date.now())));
      // Browser timer is bounded; it carries only a message key, never the code.
      void timer;
    }
    if (!sensitive && (replyText || rich)) {
      // Her voice and the live region get the bubbles only (never the widgets), without markdown.
      const spoken = rich ? speechText(rich) : stripMarkdown(replyText);
      setLastReply(spoken);
      for (const fn of replyListeners) fn(spoken);
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
 * The server accepts 1-12000 characters with no control characters other than
 * newline and tab.
 */
export function cleanMessage(text: string): string {
  return text.replace(/\r\n?/g, "\n").replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/g, "").trim();
}

/** Adds an optimistic user bubble and sends it. Returns false if nothing was sent. */
export async function sendMessage(text: string): Promise<boolean> {
  const message = cleanMessage(text);
  // `/secret NAME VALUE` opens the Add secret sheet (views/Chat.tsx); it is
  // never a chat message, so it never gets a bubble, the cache or the server.
  if (isSecretCommand(message)) return false;
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
    replyTo: replyTarget()?.id,
  };
  setReplyTarget(null);
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
  if (msg.share) {
    setChat("messages", (m) => m.key === key, { status: "sending", error: undefined, share: { ...msg.share!, phase: "uploading", progress: 0, error: undefined } });
    persist();
    await deliverShare(key);
    return;
  }
  setChat("messages", (m) => m.key === key, { status: "sending", error: undefined });
  persist();
  await deliver(key);
}

/**
 * Adds an optimistic attachment bubble (files and/or a shared location) and
 * sends it through POST /v1/share. Returns false if there is nothing to send.
 */
export async function sendAttachmentMessage(
  files: readonly DraftAttachment[],
  caption = "",
  location?: ShareLocation | null,
): Promise<boolean> {
  const text = cleanMessage(caption);
  if (text.length > CHAT_MAX_CHARS) return false;
  if (!files.length && !location) return false;
  if (sending()) return false;
  const shareId = newShareId();
  const last = chat.messages[chat.messages.length - 1];
  const attachments = files.map(draftAttachment);
  if (location) attachments.push(locationAttachment(location));
  const msg: ChatMessage = {
    key: `f:${shareId}`,
    role: "user",
    text,
    createdAt: Math.max(Date.now(), last ? last.createdAt + 1 : 0),
    channel: "web",
    status: "sending",
    attachments,
    share: {
      id: shareId,
      files: [...files],
      progress: 0,
      phase: "uploading",
      meta: buildShareMeta(files, location),
    },
  };
  commit((list) => {
    list.push(msg);
  });
  await deliverShare(msg.key);
  return true;
}

function setShareProgress(key: string, progress: number): void {
  setChat(
    "messages",
    (m) => m.key === key,
    "share",
    (share) => (share ? { ...share, progress: Math.max(share.progress, Math.min(1, progress)) } : share),
  );
}

async function deliverShare(key: string): Promise<void> {
  const msg = find(key);
  if (!msg?.share) return;
  const share = msg.share;
  const shareId = share.id;
  const files = share.files;
  const meta = share.meta;
  const text = msg.text;
  const gen = generation;
  setSending(true);
  try {
    const accepted = await postChatShare(
      { shareId, files, caption: text, meta },
      { onProgress: (loaded, total) => setShareProgress(key, total > 0 ? loaded / total : 0) },
    );
    if (gen !== generation || !find(key)) return;
    let finished = false;
    commit((list) => {
      const mine = list.find((m) => m.key === key);
      if (!mine) return;
      mine.status = "sent";
      delete mine.error;
      if (accepted.userItemId) adopt(list, key, accepted.userItemId);
      const replied = !!mine.itemId &&
        list.some((m) => m.role === "lucina" && (m.replyTo === mine.itemId || (!!accepted.lucinaItemId && m.itemId === accepted.lucinaItemId)));
      if ((replied || accepted.stage === "done") && mine.share) finished = true;
    });
    // Update the nested share through the store setter so the bubble re-renders
    // (a plain mutation inside commit's cloned list is not deep-merged).
    setChat(
      "messages",
      (m) => m.key === key,
      "share",
      (current): ChatShare | undefined =>
        current
          ? {
              ...current,
              progress: 1,
              phase: finished ? "done" : accepted.stage ?? "received",
              stage: finished ? "done" : accepted.stage,
              error: undefined,
            }
          : current,
    );
    if (!finished) startSharePoll(key, shareId);
  } catch (e) {
    if (gen !== generation || !find(key)) return;
    const message = e instanceof Error && e.message ? e.message : t("chat.notDelivered");
    setChat("messages", (m) => m.key === key, { status: "error", error: t("share.notSentRetry") });
    setChat("messages", (m) => m.key === key, "share", (current): ChatShare | undefined =>
      current ? { ...current, phase: "error", error: message } : current);
    persist();
  } finally {
    if (gen === generation) setSending(false);
  }
}

// ---- Share status polling ------------------------------------------------------

const shareStops = new Map<string, () => void>();

function stopSharePoll(shareId: string): void {
  const stop = shareStops.get(shareId);
  if (!stop) return;
  shareStops.delete(shareId);
  stop();
}

function startSharePoll(key: string, shareId: string): void {
  stopSharePoll(shareId);
  const stop = pollShareStatus(shareId, {
    onUpdate: (status) => {
      if (!find(key)) {
        stopSharePoll(shareId);
        return;
      }
      applyShareStatus(key, status);
      if (status.stage === "done" || status.stage === "error") shareStops.delete(shareId);
    },
  });
  shareStops.set(shareId, stop);
}

function applyShareStatus(key: string, status: ShareStatus): void {
  commit((list) => {
    const mine = list.find((m) => m.key === key);
    if (!mine?.share) return;
    // Replace (never mutate) the nested share and attachments: commit's copy is
    // shallow, and an in-place edit would bypass the store's signals.
    mine.share = {
      ...mine.share,
      phase: status.stage,
      stage: status.stage,
      attachments: status.attachments,
      ...(status.error ? { error: status.error } : {}),
    };
    if (status.stage === "error") {
      mine.status = "error";
      mine.error = status.error ?? t("share.notSentRetry");
    }
    if (status.userItemId) adopt(list, key, status.userItemId);
    if (mine.attachments && status.attachments.length) {
      mine.attachments = mine.attachments.map((attachment, index) => {
        const item = status.attachments.find((a) => a.index === index);
        if (!item) return attachment;
        return {
          ...attachment,
          ...(item.mediaId ? { mediaId: item.mediaId } : {}),
          ...(item.summary && !attachment.name ? { name: item.summary } : {}),
        };
      });
    }
  });
}

export function stopSharePolling(): void {
  for (const stop of shareStops.values()) stop();
  shareStops.clear();
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
      updateJob(key, { status: "error", pollError: t("chat.jobGone") });
      return;
    }
    if (err.status === 401) return; // Session ended; the app returns to login.
    updateJob(key, { pollError: t("chat.jobRetrying") });
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
  stopSharePolling();
  for (const m of chat.messages) if (m.share) for (const file of m.share.files) releaseDraft(file);
  setChat({ messages, nextBefore: null, loaded: false });
  setSending(false);
  setLastReply("");
  setLoadState("idle");
  setOlderState("idle");
  setStreamStatus("connecting");
  firstMessageSource = "server";
}

/** Forget the conversation (logout). */
export function clearChat(): void {
  setServerCards(reconcile({}));
  reset([]);
  deviceId = undefined;
  setReplyTarget(null);
  sentClientMessageIds.clear();
  void clearChatDB();
  removeKey("session", CACHE_KEY);
}

/** Test helper: reset tab state and clean legacy storage keys. */
export function reloadChatFromStorage(): void {
  reset(hydrate());
}
