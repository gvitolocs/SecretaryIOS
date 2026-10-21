import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import { toApiError, type ApiError } from "../api/client";
import { api, CHAT_MAX_CHARS } from "../api/secretary";
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
  id: string;
  role: "user" | "lucina";
  text: string;
  createdAt: number;
  /** User messages only. */
  status?: "sending" | "sent" | "error";
  error?: string;
  action?: string | null;
  job?: JobState | null;
}

const STORAGE_KEY = "secretary.chat.v1";
export const JOB_POLL_MS = 3_000;

export function isTerminalJob(status: string): boolean {
  return status === "done" || status === "error";
}

function hydrate(): ChatMessage[] {
  const data = readJSON("session", STORAGE_KEY);
  if (!Array.isArray(data)) return [];
  const out: ChatMessage[] = [];
  for (const m of data) {
    if (!m || typeof m !== "object") continue;
    const msg = m as ChatMessage;
    if (typeof msg.id !== "string" || typeof msg.text !== "string") continue;
    if (msg.role !== "user" && msg.role !== "lucina") continue;
    // A reload interrupted this send; let the user retry it.
    if (msg.status === "sending") {
      out.push({ ...msg, status: "error", error: "Not sent: the page was reloaded." });
    } else {
      out.push(msg);
    }
  }
  return out;
}

const [chat, setChat] = createStore<{ messages: ChatMessage[] }>({ messages: hydrate() });
const [sending, setSending] = createSignal(false);
/** Latest Lucina reply, for the screen-reader live region. Not set on hydrate. */
const [lastReply, setLastReply] = createSignal("");

export { chat, sending, lastReply };

function persist(): void {
  writeJSON("session", STORAGE_KEY, chat.messages);
}

function find(id: string): ChatMessage | undefined {
  return chat.messages.find((m) => m.id === id);
}

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

async function deliver(id: string, text: string): Promise<void> {
  setSending(true);
  try {
    const res = await api.sendChat({ message: text, sessionId: chatSessionId(), clientMessageId: id });
    markLucinaOnline();
    if (!find(id)) return; // Cleared (e.g. logout) while in flight.
    const job = res?.job && res.job.id !== undefined && res.job.id !== null ? res.job : null;
    const reply: ChatMessage = {
      id: uuid(),
      role: "lucina",
      text: typeof res?.reply === "string" ? res.reply : "",
      createdAt: Date.now(),
      action: typeof res?.action === "string" ? res.action : null,
      job: job
        ? {
            id: String(job.id),
            status: String(job.status ?? "queued"),
            action: typeof res.action === "string" ? res.action : null,
            pollError: null,
          }
        : null,
    };
    setChat(
      "messages",
      produce((list) => {
        const mine = list.find((m) => m.id === id);
        if (mine) {
          mine.status = "sent";
          delete mine.error;
        }
        list.push(reply);
      }),
    );
    persist();
    if (reply.text) setLastReply(reply.text);
    if (reply.job) scheduleJobPoll(reply.id, isTerminalJob(reply.job.status) ? 0 : JOB_POLL_MS);
  } catch (e) {
    const err = toApiError(e);
    if (!find(id)) return;
    setChat("messages", (m) => m.id === id, { status: "error", error: describeChatError(err) });
    persist();
  } finally {
    setSending(false);
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
  const msg: ChatMessage = { id: uuid(), role: "user", text: message, createdAt: Date.now(), status: "sending" };
  setChat("messages", chat.messages.length, msg);
  persist();
  await deliver(msg.id, message);
  return true;
}

/** Resends a failed message with the same clientMessageId (idempotent on the server). */
export async function retryMessage(id: string): Promise<void> {
  const msg = find(id);
  if (!msg || msg.role !== "user" || msg.status !== "error" || sending()) return;
  const text = msg.text;
  setChat("messages", (m) => m.id === id, { status: "sending", error: undefined });
  persist();
  await deliver(id, text);
}

// ---- Job polling -----------------------------------------------------------

const timers = new Map<string, ReturnType<typeof setTimeout>>();

function scheduleJobPoll(messageId: string, delay = JOB_POLL_MS): void {
  const existing = timers.get(messageId);
  if (existing !== undefined) clearTimeout(existing);
  timers.set(
    messageId,
    setTimeout(() => {
      void pollJob(messageId);
    }, delay),
  );
}

function updateJob(messageId: string, patch: Partial<JobState>): void {
  setChat("messages", (m) => m.id === messageId && !!m.job, "job", (job) => (job ? { ...job, ...patch } : job));
  persist();
}

async function pollJob(messageId: string): Promise<void> {
  timers.delete(messageId);
  const jobId = find(messageId)?.job?.id;
  if (!jobId) return;
  try {
    const info = await api.getJob(jobId);
    if (!find(messageId)) return;
    const status = String(info?.status ?? "queued");
    const patch: Partial<JobState> = { status, pollError: null };
    if (typeof info?.action === "string") patch.action = info.action;
    if (info && "result" in info) patch.result = info.result;
    if (typeof info?.error === "string") patch.error = info.error;
    if (typeof info?.imageUrl === "string") patch.imageUrl = info.imageUrl;
    updateJob(messageId, patch);
    if (!isTerminalJob(status)) scheduleJobPoll(messageId);
  } catch (e) {
    const err = toApiError(e);
    if (!find(messageId)) return;
    if (err.status === 404 || err.code === "job_not_found") {
      updateJob(messageId, { status: "error", pollError: "This job no longer exists." });
      return;
    }
    if (err.status === 401) return; // Session ended; the app returns to login.
    updateJob(messageId, { pollError: "Couldn't check on this job. Retrying…" });
    scheduleJobPoll(messageId);
  }
}

/** Restart polling for unfinished jobs (after a reload or a remount). */
export function resumeJobPolling(): void {
  for (const m of chat.messages) {
    if (m.job && !isTerminalJob(m.job.status) && !timers.has(m.id)) scheduleJobPoll(m.id, 0);
  }
}

export function stopJobPolling(): void {
  for (const t of timers.values()) clearTimeout(t);
  timers.clear();
}

/** Forget the conversation (logout). */
export function clearChat(): void {
  stopJobPolling();
  setChat("messages", []);
  setLastReply("");
  removeKey("session", STORAGE_KEY);
}

/** Test helper: reload state from sessionStorage. */
export function reloadChatFromStorage(): void {
  stopJobPolling();
  setChat("messages", hydrate());
  setSending(false);
  setLastReply("");
}
