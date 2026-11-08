import type { AgentListItem, AgentSnapshot, PermissionView, TimelineRow, WorkspaceRecord } from "../api/agents";
import { stubFetch, type Call } from "./fetch";

// A fake lucina-agentd behind /v1/agents, for the Agents view tests and the screenshot mock.

export const NOW = Date.parse("2026-10-28T12:00:00Z");
const iso = (minAgo: number) => new Date(NOW - minAgo * 60_000).toISOString();

export const item = (id: string, over: Partial<AgentListItem> = {}): AgentListItem => ({
  id,
  shortId: id.slice(0, 7),
  title: `Agent ${id}`,
  provider: "claude",
  model: "opus",
  status: "idle",
  cwd: "/home/nez/Projects/demo",
  createdAt: iso(60),
  updatedAt: iso(5),
  lastUserMessageAt: iso(6),
  archivedAt: null,
  bucket: "done",
  labels: {},
  ...over,
});

export const snapshot = (id: string, over: Partial<AgentSnapshot> = {}): AgentSnapshot => ({
  id,
  provider: "claude",
  cwd: "/home/nez/Projects/demo",
  workspaceId: "wks_1",
  model: "opus",
  createdAt: iso(60),
  updatedAt: iso(5),
  status: "idle",
  pendingPermissions: [],
  title: `Agent ${id}`,
  labels: {},
  ...over,
});

export const workspace = (over: Partial<WorkspaceRecord> = {}): WorkspaceRecord => ({
  workspaceId: "wks_1",
  projectId: "prj_1",
  cwd: "/home/nez/Projects/demo",
  name: "main",
  projectDisplayName: "demo",
  isolation: "local",
  ...over,
});

export const row = (seq: number, item: TimelineRow["item"], turnId = "t1"): TimelineRow => ({ seq, timestamp: iso(5), item, turnId });

export const permission = (over: Partial<PermissionView> = {}): PermissionView => ({
  id: "req_1",
  kind: "agent",
  agentId: "a1",
  name: "Bash",
  title: "Run: npm test",
  details: { command: "npm test" },
  status: "pending",
  createdAt: iso(1),
  expiresAt: new Date(Date.now() + 5 * 60_000).toISOString(),
  ...over,
});

export interface Fake {
  items?: AgentListItem[];
  archived?: AgentListItem[];
  snapshots?: Record<string, AgentSnapshot>;
  timelines?: Record<string, { epoch?: string; rows: TimelineRow[] }>;
  permissions?: PermissionView[];
  workspaces?: WorkspaceRecord[];
  /** Routes that answer 404 (not deployed). */
  absent?: string[];
  /** Extra routes: matched first. Return undefined to fall through. */
  extra?: (call: Call) => { status?: number; body?: unknown } | undefined;
}

/** Stub fetch with a fake /v1/agents. Returns the recorded calls. */
export function serveAgents(f: Fake = {}): Call[] {
  return stubFetch((call) => {
    const extra = f.extra?.(call);
    if (extra) return extra;
    const url = new URL(call.url, "http://x");
    const path = url.pathname.replace(/^\/v1\/agents/, "") || "/";
    if (f.absent?.some((a) => path === a || path.startsWith(`${a}/`) || (a.endsWith("*") && path.startsWith(a.slice(0, -1))))) {
      return { status: 404, body: { error: { code: "NOT_FOUND", message: "Not found" } } };
    }
    if (path === "/items") return { body: { agents: url.searchParams.get("archived") === "true" ? (f.archived ?? []) : (f.items ?? []) } };
    if (path === "/permissions") return { body: { permissions: f.permissions ?? [] } };
    if (path === "/workspaces" && call.method === "GET") return { body: { workspaces: f.workspaces ?? [workspace()] } };
    if (path === "/projects") return { body: { projects: [{ projectId: "prj_1", rootPath: "/home/nez/Projects/demo", name: "demo" }] } };
    if (path === "/providers")
      return {
        body: [
          { id: "claude", label: "Claude", defaultModeId: "default", modes: [{ id: "default", label: "Default" }, { id: "acceptEdits", label: "Accept edits" }], models: [{ id: "opus", label: "Opus", isDefault: true, isSelectable: true }] },
          { id: "codex", label: "Codex", modes: [], models: [] },
        ],
      };
    const t = /^\/([^/]+)\/timeline$/.exec(path);
    if (t) {
      const tl = f.timelines?.[t[1]!] ?? { rows: [] };
      const after = Number(url.searchParams.get("afterSeq") ?? 0);
      const rows = tl.rows.filter((r) => r.seq > after);
      return { body: { epoch: tl.epoch ?? "e1", rows, nextSeq: rows.length ? rows[rows.length - 1]!.seq : after } };
    }
    const s = /^\/([A-Za-z0-9_-]+)$/.exec(path);
    if (s && call.method === "GET" && f.snapshots?.[s[1]!]) return { body: f.snapshots[s[1]!] };
    if (s && call.method === "GET") return { status: 404, body: { error: { code: "AGENT_NOT_FOUND", message: "Agent not found" } } };
    return { status: 404, body: { error: { code: "NOT_FOUND", message: `no fake for ${call.method} ${path}` } } };
  });
}
