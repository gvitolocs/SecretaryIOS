import { createSignal } from "solid-js";
import { toApiError, type ApiError } from "../api/client";
import { tasksApi, type Parsed, type TaskCommand, type TasksResponse, type TasksView, type ViewQuery } from "../api/tasks";
import { uuid } from "../lib/ids";
import { readJSON, writeJSON } from "../lib/storage";
import { currentTimeZone } from "./engine";

// Tasks state: the open list, commands, and the offline outbox.
//
// Commands carry an op_id and no expected_revision (task commands each
// target one task; the engine allows it). A command that cannot reach the
// server is kept in localStorage with its op_id and sent again, in order,
// when the connection is back; the engine's receipts make a replay
// harmless. While queued, completions hide the task and additions show as
// waiting rows.

export const OUTBOX_KEY = "secretary.tasks.outbox";
const POLL_MS = 60_000;
const RETRY_MS = 20_000;
const TOAST_MS = 7_000;

export interface Queued {
  op_id: string;
  command: TaskCommand;
  at: number;
}

export interface Toast {
  text: string;
  undo?: TaskCommand;
}

const [view, setView] = createSignal<TasksView | null>(null);
const [query, setQueryState] = createSignal<ViewQuery>({ kind: "today" });
const [loading, setLoading] = createSignal(false);
const [loadError, setLoadError] = createSignal<string | null>(null);
const [actionError, setActionError] = createSignal<string | null>(null);
const [outbox, setOutbox] = createSignal<Queued[]>(readOutbox());
const [toast, setToast] = createSignal<Toast | null>(null);
const [syncing, setSyncing] = createSignal(false);

export { actionError, loadError, loading, outbox, query, setActionError, syncing, toast, view };

function readOutbox(): Queued[] {
  const v = readJSON("local", OUTBOX_KEY);
  return Array.isArray(v) ? (v.filter((x) => x && typeof x === "object" && typeof x.op_id === "string") as Queued[]) : [];
}

function saveOutbox(q: Queued[]): void {
  setOutbox(q);
  writeJSON("local", OUTBOX_KEY, q);
}

function offline(e: ApiError): boolean {
  return e.status === 0 || e.status === 502 || e.status === 503 || e.status === 504;
}

let generation = 0;
/** Bumped by resetTasksState: answers to older commands are ignored. */
let epoch = 0;

/** Show a list (and remember which one is open). */
export async function load(q: ViewQuery = query()): Promise<void> {
  const mine = ++generation;
  setQueryState(q);
  setLoading(true);
  try {
    const r = await tasksApi.state(q, currentTimeZone());
    if (mine !== generation) return;
    if (r.view) setView(r.view);
    setLoadError(null);
  } catch (e) {
    if (mine !== generation) return;
    setLoadError(toApiError(e).message);
  } finally {
    if (mine === generation) setLoading(false);
  }
}

export const refresh = () => load(query());

let toastTimer: ReturnType<typeof setTimeout> | undefined;
export function showToast(t: Toast | null): void {
  clearTimeout(toastTimer);
  setToast(t);
  if (t) toastTimer = setTimeout(() => setToast(null), TOAST_MS);
}

async function send(item: Queued): Promise<TasksResponse> {
  return tasksApi.command(item.op_id, currentTimeZone(), item.command, query());
}

/**
 * Run a command. Answers the engine response, `"queued"` when it was kept
 * for later (offline), or null when the server refused it (the message is
 * in `actionError`).
 */
export async function run(command: TaskCommand): Promise<TasksResponse | "queued" | null> {
  setActionError(null);
  const mine = epoch;
  const item: Queued = { op_id: uuid(), command, at: Date.now() };
  if (outbox().length > 0) {
    // keep the order: behind what is already waiting
    saveOutbox([...outbox(), item]);
    void flush();
    return "queued";
  }
  try {
    const r = await send(item);
    if (r.view && mine === epoch) setView(r.view);
    return r;
  } catch (e) {
    if (mine !== epoch) return null;
    const err = toApiError(e);
    if (offline(err)) {
      saveOutbox([...outbox(), item]);
      scheduleRetry();
      return "queued";
    }
    if (err.status === 409) {
      // the engine only answers 409 for a stale expected_revision; refresh
      await refresh();
    }
    setActionError(err.message);
    return null;
  }
}

let flushing = false;
let retryTimer: ReturnType<typeof setTimeout> | undefined;

function scheduleRetry(): void {
  clearTimeout(retryTimer);
  retryTimer = setTimeout(() => void flush(), RETRY_MS);
}

/** Send the queued commands in order; stops at the first one that cannot reach the server. */
export async function flush(): Promise<void> {
  if (flushing) return;
  flushing = true;
  const mine = epoch;
  try {
    while (outbox().length > 0) {
      const head = outbox()[0] as Queued;
      try {
        const r = await send(head);
        if (mine !== epoch) return;
        if (r.view) setView(r.view);
      } catch (e) {
        if (mine !== epoch) return;
        const err = toApiError(e);
        if (offline(err)) {
          scheduleRetry();
          return;
        }
        // refused (the task is gone, …): drop it and say so
        setActionError(err.message);
      }
      saveOutbox(outbox().slice(1));
    }
    await refresh();
  } finally {
    flushing = false;
  }
}

/** Ask the server to sync with Todoist now, then reload. */
export async function syncNow(): Promise<string | null> {
  if (syncing()) return null;
  setSyncing(true);
  try {
    await tasksApi.sync();
    return null;
  } catch (e) {
    return toApiError(e).message;
  } finally {
    setSyncing(false);
    await refresh();
  }
}

let parseSeq = 0;
/** Quick add preview (the engine's parse, it/en/da). Stale answers are dropped. */
export async function parseQuick(text: string): Promise<Parsed | null> {
  const mine = ++parseSeq;
  if (!text.trim()) return null;
  try {
    const r = await tasksApi.parse(text, currentTimeZone());
    return mine === parseSeq ? (r.parsed ?? null) : null;
  } catch {
    return null;
  }
}

/** Task ids hidden while their completion or deletion waits in the outbox. */
export function hiddenIds(): Set<string> {
  const out = new Set<string>();
  for (const q of outbox()) {
    if (q.command.type === "complete" || q.command.type === "delete") out.add(q.command.task_id);
  }
  return out;
}

/** Additions waiting in the outbox (shown as pending rows). */
export function waitingAdds(): string[] {
  return outbox()
    .map((q) => (q.command.type === "quick_add" ? q.command.text : q.command.type === "add" ? q.command.content : null))
    .filter((x): x is string => x !== null);
}

/** Start polling and the online listener; returns the cleanup. */
export function startTasks(): () => void {
  const poll = setInterval(() => {
    if (typeof document === "undefined" || document.visibilityState === "visible") void refresh();
  }, POLL_MS);
  const onOnline = () => void flush();
  const onVisible = () => {
    if (document.visibilityState === "visible") void (outbox().length ? flush() : refresh());
  };
  window.addEventListener("online", onOnline);
  document.addEventListener("visibilitychange", onVisible);
  if (outbox().length) void flush();
  return () => {
    clearInterval(poll);
    clearTimeout(retryTimer);
    window.removeEventListener("online", onOnline);
    document.removeEventListener("visibilitychange", onVisible);
  };
}

/** Tests: forget everything. */
export function resetTasksState(): void {
  generation++;
  epoch++;
  setView(null);
  setQueryState({ kind: "today" });
  setLoadError(null);
  setActionError(null);
  saveOutbox([]);
  showToast(null);
  flushing = false;
}
