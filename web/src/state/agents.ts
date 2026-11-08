// Lucina Agents' state: the agent list, snapshots, timelines, the permission book, workspaces, terminals and
// scripts. One shared SSE (`GET /v1/agents/stream`) keeps them live. It resumes with Last-Event-ID (the browser
// does it by itself while the connection retries; after a hard failure a new EventSource gets ?lastEventId=),
// and on `stream.reset` everything is fetched again. `agent_command` frames (for external agents) are ignored.
// A stream that stays away is checked against the REST routes, which say what the page shows: nothing for a short
// drop (an agentd restart), a real error when the backend is down, or polling when only the stream is broken.

import { createSignal } from "solid-js";
import { createStore, produce, reconcile } from "solid-js/store";
import {
  agentsApi,
  AGENTS_STREAM_URL,
  notDeployed,
  type AgentListItem,
  type AgentSnapshot,
  type BusEvent,
  type PermissionView,
  type Script,
  type Terminal,
  type TimelineRow,
  type WorkspaceRecord,
} from "../api/agents";
import { toApiError } from "../api/client";
import { t } from "../i18n";
import { BUCKET_ORDER, itemFromSnapshot, mergeRows } from "../lib/agentsTimeline";

export interface Timeline {
  epoch: string;
  rows: TimelineRow[];
  status: "loading" | "ready" | "error";
  error?: string;
}

interface State {
  items: Record<string, AgentListItem>;
  snapshots: Record<string, AgentSnapshot>;
  timelines: Record<string, Timeline>;
  permissions: Record<string, PermissionView>;
  workspaces: Record<string, WorkspaceRecord>;
  terminals: Record<string, Terminal>;
  scripts: Record<string, Script[]>;
}

const empty = (): State => ({ items: {}, snapshots: {}, timelines: {}, permissions: {}, workspaces: {}, terminals: {}, scripts: {} });
const [state, setState] = createStore<State>(empty());
export { state as agentsState };

const [loaded, setLoaded] = createSignal(false);
const [loadError, setLoadError] = createSignal<string | null>(null);
const [connected, setConnected] = createSignal(false);
export { loaded as agentsLoaded, loadError as agentsError, connected as agentsConnected };

/** "live": connected, or away for less than `quietMs`. The others last until the stream is back. */
export type StreamHealth = "live" | "reconnecting" | "polling" | "down";
const [health, setHealth] = createSignal<StreamHealth>("live");
export { health as agentsHealth };

/** Phase 5/6 routes: undefined = not probed yet, false = answered 404/405/501. */
export type FeatureName = "search" | "prompts" | "schedules" | "usage" | "terminals" | "detach";
const [features, setFeatures] = createStore<Partial<Record<FeatureName, boolean>>>({});
export { features as agentFeatures };
export function markFeature(name: FeatureName, available: boolean): void {
  if (features[name] !== available) setFeatures(name, available);
}
/** Run a probe request: the feature is on when it answers, off when the route is not deployed. */
export async function detect<T>(name: FeatureName, call: () => Promise<T>): Promise<T | undefined> {
  try {
    const r = await call();
    markFeature(name, true);
    return r;
  } catch (e) {
    if (notDeployed(e)) markFeature(name, false);
    else throw e;
    return undefined;
  }
}

export const permKey = (p: { id: string; agentId?: string }): string => (p.agentId ? `${p.agentId}/${p.id}` : p.id);

// ---------------------------------------------------------------- loading

export async function loadItems(): Promise<void> {
  const list = await agentsApi.items(false);
  setState(
    "items",
    produce((draft) => {
      const seen = new Set(list.map((a) => a.id));
      for (const [id, a] of Object.entries(draft)) if (!a.archivedAt && !seen.has(id)) delete draft[id];
      for (const a of list) draft[a.id] = a;
    }),
  );
}

export async function loadArchived(): Promise<void> {
  const list = await agentsApi.items(true);
  setState(
    "items",
    produce((draft) => {
      for (const a of list) draft[a.id] = a;
    }),
  );
}

export async function loadPermissions(): Promise<void> {
  const list = await agentsApi.permissions(true);
  const next: Record<string, PermissionView> = {};
  for (const p of list) next[permKey(p)] = p;
  setState("permissions", next);
}

export async function loadWorkspaces(): Promise<void> {
  const list = await agentsApi.workspaces();
  const next: Record<string, WorkspaceRecord> = {};
  for (const w of list) next[w.workspaceId] = w;
  setState("workspaces", next);
}

export async function loadSnapshot(id: string): Promise<AgentSnapshot | null> {
  try {
    const s = await agentsApi.snapshot(id);
    applySnapshot(s);
    return s;
  } catch (e) {
    if (toApiError(e).status === 404) return null;
    throw e;
  }
}

const dirty = new Set<string>();

/** Fetch an agent's timeline (all of it on the first call, then only what is new). */
export async function loadTimeline(id: string): Promise<void> {
  const cur = state.timelines[id];
  if (cur?.status === "loading") return;
  const gen = generation;
  if (!cur?.rows) setState("timelines", id, { epoch: "", rows: [], status: "loading" });
  else setState("timelines", id, "status", "loading");
  try {
    for (let guard = 0; guard < 60; guard++) {
      dirty.delete(id);
      const tl = state.timelines[id]!;
      const after = tl.rows.length ? tl.rows[tl.rows.length - 1]!.seq : 0;
      const page = await agentsApi.timeline(id, after, 500);
      if (gen !== generation || !state.timelines[id]) return;
      if (tl.epoch && page.epoch !== tl.epoch && tl.rows.length) {
        // The timeline was rebuilt: start over.
        setState("timelines", id, { epoch: page.epoch, rows: [], status: "loading" });
        continue;
      }
      setState("timelines", id, "epoch", page.epoch);
      setState("timelines", id, "rows", (r) => mergeRows(r, page.rows));
      if (page.rows.length < 500 && !dirty.has(id)) break;
    }
    setState("timelines", id, "status", "ready");
  } catch (e) {
    if (gen === generation && state.timelines[id]) setState("timelines", id, { status: "error", error: toApiError(e).message });
  }
}

export async function refreshAll(): Promise<void> {
  const results = await Promise.allSettled([loadItems(), loadPermissions(), loadWorkspaces()]);
  const failed = results.find((r) => r.status === "rejected") as PromiseRejectedResult | undefined;
  setLoadError(failed ? toApiError(failed.reason).message : null);
  setLoaded(true);
  // Whatever was open is brought up to date too.
  for (const id of Object.keys(state.snapshots)) void loadSnapshot(id).catch(() => undefined);
  for (const id of Object.keys(state.timelines)) void loadTimeline(id);
  if (features.terminals) void loadTerminals().catch(() => undefined);
}

export async function loadTerminals(): Promise<void> {
  const list = await detect("terminals", () => agentsApi.terminals());
  if (!list) return;
  const next: Record<string, Terminal> = {};
  for (const x of list) next[x.id] = x;
  setState("terminals", next);
}

export async function loadScripts(workspaceId: string): Promise<boolean> {
  try {
    setState("scripts", workspaceId, await agentsApi.scripts(workspaceId));
    return true;
  } catch (e) {
    if (notDeployed(e)) return false;
    throw e;
  }
}

// ---------------------------------------------------------------- applying events

export function applySnapshot(s: AgentSnapshot): void {
  setState("snapshots", s.id, s);
  setState("items", s.id, itemFromSnapshot(s));
}

/** Apply one event of the shared stream. */
export function applyEvent(ev: BusEvent): void {
  switch (ev.type) {
    case "agent_state":
      applySnapshot((ev as { agent: AgentSnapshot }).agent);
      return;
    case "agent_deleted": {
      const id = (ev as { agentId: string }).agentId;
      setState(
        produce((s) => {
          delete s.items[id];
          delete s.snapshots[id];
          delete s.timelines[id];
        }),
      );
      return;
    }
    case "permission": {
      const p = (ev as { permission: PermissionView }).permission;
      setState("permissions", permKey(p), p);
      return;
    }
    case "workspace": {
      const w = (ev as { workspace: WorkspaceRecord }).workspace;
      setState("workspaces", w.workspaceId, (old) => ({ ...(old ?? {}), ...w }));
      return;
    }
    case "terminal": {
      const x = (ev as { terminal: Terminal }).terminal;
      setState("terminals", x.id, x);
      return;
    }
    case "terminal_removed":
      setState("terminals", produce((s) => void delete s[(ev as { terminalId: string }).terminalId]));
      return;
    case "script_status": {
      const e = ev as { workspaceId: string; scripts: Script[] };
      setState("scripts", e.workspaceId, e.scripts);
      return;
    }
    case "agent_stream":
      applyAgentStream(ev as Extract<BusEvent, { type: "agent_stream" }>);
      return;
    default:
      // agent_command (for external agents' owner devices), terminal_attention and anything new.
      return;
  }
}

function applyAgentStream(ev: Extract<BusEvent, { type: "agent_stream" }>): void {
  const tl = state.timelines[ev.agentId];
  if (!tl || ev.event.type !== "timeline" || ev.seq === undefined || !ev.event.item) return;
  if (tl.status === "loading") {
    dirty.add(ev.agentId);
    return;
  }
  if (tl.epoch && ev.epoch !== tl.epoch) {
    void loadTimeline(ev.agentId);
    return;
  }
  const last = tl.rows.length ? tl.rows[tl.rows.length - 1]!.seq : 0;
  if (ev.seq <= last) return;
  if (last > 0 && ev.seq > last + 1) {
    void loadTimeline(ev.agentId);
    return;
  }
  const turnId = typeof ev.event.turnId === "string" ? ev.event.turnId : null;
  setState("timelines", ev.agentId, "rows", tl.rows.length, { seq: ev.seq, timestamp: ev.timestamp, item: ev.event.item, turnId });
}

// ---------------------------------------------------------------- the stream

let users = 0;
/** Bumped by resetAgentsState: a cleanup from before it must not count again. */
let generation = 0;
let source: EventSource | null = null;
let lastId = "";
let retryTimer: ReturnType<typeof setTimeout> | undefined;
let lostTimer: ReturnType<typeof setTimeout> | undefined;
let pollTimer: ReturnType<typeof setTimeout> | undefined;
let retryMs = 0;
let attemptAt = 0;
let hiddenAt = 0;

/** Exported so tests can shorten them. */
export const streamTiming = {
  /** A restart is back within this, so the page says nothing until then. */
  quietMs: 5000,
  /** How often the REST routes are read while the stream is away. */
  pollMs: 15000,
  /** The first retry after the server refused a connection, doubled up to `retryMaxMs`. */
  retryMs: 3000,
  retryMaxMs: 15000,
  /** A connection attempt that has not answered in this long is started over. */
  stuckMs: 30000,
  /** A page hidden this long (a Mac asleep) may hold a dead connection, so it opens a new one. */
  wakeMs: 60000,
};

function parse(data: string): BusEvent | null {
  try {
    const v = JSON.parse(data) as unknown;
    return v && typeof v === "object" && typeof (v as { type?: unknown }).type === "string" ? (v as BusEvent) : null;
  } catch {
    return null;
  }
}

function openStream(): void {
  if (typeof EventSource === "undefined") return;
  const url = lastId ? `${AGENTS_STREAM_URL}?lastEventId=${encodeURIComponent(lastId)}` : AGENTS_STREAM_URL;
  const es = new EventSource(url, { withCredentials: true });
  source = es;
  attemptAt = Date.now();
  if (!connected()) markLost();
  es.onopen = markLive;
  es.addEventListener("message", (m) => {
    const ev = m as MessageEvent<string>;
    if (ev.lastEventId) lastId = ev.lastEventId;
    const body = parse(ev.data);
    if (!body) return;
    if (body.type === "stream.reset") {
      void refreshAll();
      return;
    }
    applyEvent(body);
  });
  es.addEventListener("stream.reset", (m) => {
    const ev = m as MessageEvent<string>;
    // The reset frame carries an id to resume from.
    if (ev.lastEventId) lastId = ev.lastEventId;
    void refreshAll();
  });
  es.onerror = () => {
    setConnected(false);
    markLost();
    attemptAt = Date.now();
    // CONNECTING: the browser retries by itself and sends Last-Event-ID. CLOSED: we start over.
    if (es.readyState === 2) {
      es.close();
      if (source === es) source = null;
      clearTimeout(retryTimer);
      retryMs = retryMs ? Math.min(retryMs * 2, streamTiming.retryMaxMs) : streamTiming.retryMs;
      retryTimer = setTimeout(() => {
        if (users > 0 && !source) openStream();
      }, retryMs);
    }
  };
}

function markLive(): void {
  const wasAway = health() !== "live";
  clearTimeout(lostTimer);
  clearTimeout(pollTimer);
  lostTimer = pollTimer = undefined;
  retryMs = 0;
  setConnected(true);
  setHealth("live");
  // The replay covers what the stream missed; this clears the error the page was showing.
  if (wasAway || loadError()) void refreshAll();
}

function markLost(): void {
  if (health() === "live" && lostTimer === undefined) lostTimer = setTimeout(() => void checkBackend(), streamTiming.quietMs);
}

/** The stream has been away for a while: the REST routes tell a broken stream from a backend that is down. */
async function checkBackend(): Promise<void> {
  const gen = generation;
  lostTimer = undefined;
  if (users === 0 || connected()) return;
  if (health() === "live") setHealth("reconnecting");
  await refreshAll();
  if (gen !== generation || users === 0 || connected()) return;
  setHealth(loadError() ? "down" : "polling");
  if (source && source.readyState !== 1 && Date.now() - attemptAt > streamTiming.stuckMs) restartStream();
  clearTimeout(pollTimer);
  pollTimer = setTimeout(() => void checkBackend(), streamTiming.pollMs);
}

function restartStream(): void {
  source?.close();
  source = null;
  clearTimeout(retryTimer);
  retryMs = 0;
  setConnected(false);
  openStream();
}

/** Back from a hidden page or from offline: don't wait for the next retry, or for a connection that may be dead. */
function onWake(): void {
  if (users === 0) return;
  if (document.visibilityState === "hidden") {
    hiddenAt = Date.now();
    return;
  }
  const slept = hiddenAt > 0 && Date.now() - hiddenAt > streamTiming.wakeMs;
  hiddenAt = 0;
  if (!connected() || slept) restartStream();
}

/** Keep the stream open while a view needs it (reference counted). Returns the cleanup. */
export function startAgents(): () => void {
  users++;
  if (users === 1) {
    void refreshAll();
    openStream();
    document.addEventListener("visibilitychange", onWake);
    window.addEventListener("online", onWake);
  }
  let done = false;
  const mine = generation;
  return () => {
    if (done || mine !== generation) return;
    done = true;
    users--;
    if (users === 0) stopStream();
  };
}

function stopStream(): void {
  clearTimeout(retryTimer);
  clearTimeout(lostTimer);
  clearTimeout(pollTimer);
  lostTimer = pollTimer = undefined;
  retryMs = 0;
  hiddenAt = 0;
  document.removeEventListener("visibilitychange", onWake);
  window.removeEventListener("online", onWake);
  source?.close();
  source = null;
  setConnected(false);
  setHealth("live");
}

/** Tests: forget everything. */
export function resetAgentsState(): void {
  stopStream();
  users = 0;
  generation++;
  lastId = "";
  dirty.clear();
  setState(reconcile(empty()));
  setFeatures({ search: undefined, prompts: undefined, schedules: undefined, usage: undefined, terminals: undefined, detach: undefined });
  setLoaded(false);
  setLoadError(null);
}

// ---------------------------------------------------------------- derived

/** The list: needs input first, then failed, running, attention, done; newest first inside a group. */
export function agentList(archived: boolean): AgentListItem[] {
  return Object.values(state.items)
    .filter((a) => !!a.archivedAt === archived && !a.labels?.["lucina.internal"])
    .sort((a, b) => BUCKET_ORDER[a.bucket] - BUCKET_ORDER[b.bucket] || b.updatedAt.localeCompare(a.updatedAt));
}

/** What the page says about the connection, or null while there is nothing to say. */
export function agentsBanner(): { text: string; alert: boolean } | null {
  switch (health()) {
    case "reconnecting":
      return { text: t("agents.reconnecting"), alert: false };
    case "polling":
      return { text: t("agents.livePaused"), alert: false };
    case "down":
      return { text: t("agents.backendDown"), alert: true };
    default:
      return null;
  }
}

export function pendingPermissions(): PermissionView[] {
  return Object.values(state.permissions)
    .filter((p) => p.status === "pending")
    .sort((a, b) => a.createdAt.localeCompare(b.createdAt));
}

export function permissionsFor(agentId: string): PermissionView[] {
  return pendingPermissions().filter((p) => p.agentId === agentId);
}

export function workspaceOf(agentId: string): WorkspaceRecord | undefined {
  const snap = state.snapshots[agentId];
  const item = state.items[agentId];
  if (snap?.workspaceId && state.workspaces[snap.workspaceId]) return state.workspaces[snap.workspaceId];
  const cwd = snap?.cwd ?? item?.cwd;
  return cwd ? Object.values(state.workspaces).find((w) => w.cwd === cwd) : undefined;
}

// ---------------------------------------------------------------- actions (each returns an error message or null)

async function act(run: () => Promise<unknown>): Promise<string | null> {
  try {
    await run();
    return null;
  } catch (e) {
    return toApiError(e).message;
  }
}

export const sendPrompt = (id: string, prompt: string, steer: boolean) => act(() => agentsApi.send(id, prompt, steer));
export const stopAgent = (id: string) => act(() => agentsApi.stop(id));
export const archiveAgent = (id: string) => act(async () => {
  await agentsApi.archive(id);
  await loadSnapshot(id);
});
export const unarchiveAgent = (id: string) => act(async () => applySnapshot(await agentsApi.unarchive(id)));
export const renameAgent = (id: string, title: string) => act(async () => applySnapshot(await agentsApi.rename(id, title)));
export async function detachAgent(id: string): Promise<string | null> {
  try {
    applySnapshot(await agentsApi.detach(id));
    markFeature("detach", true);
    return null;
  } catch (e) {
    if (notDeployed(e)) {
      markFeature("detach", false);
      return t("agents.notAvailable");
    }
    return toApiError(e).message;
  }
}

export interface PermissionOutcome {
  ok: boolean;
  /** The request is no longer pending (answered elsewhere, expired, gone): the card should stop offering buttons. */
  closed: boolean;
  message?: string;
}

/** Allow or deny a pending request. 403/404/409/410 are told apart; they close the card. */
export async function answerPermission(p: PermissionView, behavior: "allow" | "deny"): Promise<PermissionOutcome> {
  try {
    if (p.agentId) await agentsApi.answerAgentPermission(p.agentId, p.id, behavior);
    else await agentsApi.answerExternalPermission(p.id, behavior);
    setState("permissions", permKey(p), (old) => (old && old.status === "pending" ? { ...old, status: behavior === "allow" ? "allowed" : "denied" } : old));
    return { ok: true, closed: true };
  } catch (e) {
    const err = toApiError(e);
    if (err.status === 403) return { ok: false, closed: false, message: t("agents.permForbidden") };
    if (err.status === 409) return refreshed({ ok: false, closed: true, message: t("agents.permAnswered") });
    if (err.status === 410) return refreshed({ ok: false, closed: true, message: t("agents.permExpired") });
    if (err.status === 404) return refreshed({ ok: false, closed: true, message: t("agents.permGone") });
    return { ok: false, closed: false, message: err.message };
  }
}

function refreshed(o: PermissionOutcome): PermissionOutcome {
  void loadPermissions().catch(() => undefined);
  return o;
}
