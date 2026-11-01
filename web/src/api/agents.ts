import { ApiError, request } from "./client";

// Lucina Agents (/v1/agents, owner devices): coding agents on nezopt. secretary-server proxies the routes of
// lucina-agentd (contract: docs/design/agents-api.md on server-live; wire types: crates/lucina-agents). JSON is
// camelCase, the shapes are Paseo 0.13.2's plus Lucina's fields (gate, workspaceId, PermissionView). Errors are
// {error:{code,message}}, which `request` turns into an ApiError.
//
// Phase 5 (terminals, scripts, services) and phase 6 (search, prompts, schedules, usage, detach) are feature-detected:
// a 404/405/501 means "not deployed", and the UI hides that pane.

const B = "/v1/agents";
const enc = encodeURIComponent;

export type AgentStatus = "initializing" | "idle" | "running" | "error" | "closed";
export type Bucket = "needs_input" | "failed" | "running" | "attention" | "done";
export type AttentionReason = "finished" | "error" | "permission";

export interface Usage {
  inputTokens?: number;
  cachedInputTokens?: number;
  outputTokens?: number;
  totalCostUsd?: number;
  contextWindowMaxTokens?: number;
  contextWindowUsedTokens?: number;
}

export interface AgentListItem {
  id: string;
  shortId: string;
  title: string | null;
  provider: string;
  model?: string | null;
  status: AgentStatus;
  cwd: string;
  createdAt: string;
  updatedAt: string;
  lastUserMessageAt?: string | null;
  archivedAt?: string | null;
  requiresAttention?: boolean;
  attentionReason?: AttentionReason;
  attentionTimestamp?: string;
  labels?: Record<string, string>;
  bucket: Bucket;
  external?: boolean;
  host?: string;
}

export interface ToolCallDetail {
  type: string;
  [k: string]: unknown;
}

export interface PermissionAction {
  id: string;
  label: string;
  behavior: "allow" | "deny";
  variant?: "primary" | "secondary" | "danger";
}

/** Paseo's PermissionRequest, as listed in a snapshot's pendingPermissions. */
export interface PermissionRequest {
  id: string;
  provider: string;
  name: string;
  kind: "tool" | "plan" | "question" | "mode" | "other";
  title?: string;
  description?: string;
  input?: Record<string, unknown>;
  detail?: ToolCallDetail;
  actions?: PermissionAction[];
}

export interface GateResult {
  verdict: "running" | "verified" | "refused" | "suspicious" | "unverified";
  details?: string[];
  tampered?: boolean;
  proofCommand?: string;
  round?: number;
  finishedAt?: string;
}

export interface AgentSnapshot {
  id: string;
  provider: string;
  cwd: string;
  workspaceId?: string;
  model: string | null;
  createdAt: string;
  updatedAt: string;
  lastUserMessageAt?: string | null;
  status: AgentStatus;
  currentModeId?: string | null;
  availableModes?: { id: string; label: string }[];
  pendingPermissions: PermissionRequest[];
  lastUsage?: Usage;
  lastError?: string;
  title: string | null;
  labels?: Record<string, string>;
  requiresAttention?: boolean;
  attentionReason?: AttentionReason;
  attentionTimestamp?: string;
  archivedAt?: string | null;
  gate?: GateResult;
  external?: boolean;
  host?: string;
}

export interface TodoEntry {
  text: string;
  completed: boolean;
  status?: "pending" | "in_progress" | "completed";
}

export interface TimelineItem {
  type: string;
  text?: string;
  messageId?: string | null;
  callId?: string;
  name?: string;
  detail?: ToolCallDetail;
  status?: string;
  error?: unknown;
  metadata?: Record<string, unknown>;
  items?: TodoEntry[];
  message?: string;
  level?: string;
  [k: string]: unknown;
}

export interface TimelineRow {
  seq: number;
  timestamp: string;
  item: TimelineItem;
  turnId?: string | null;
}

export interface TimelinePage {
  epoch: string;
  rows: TimelineRow[];
  nextSeq: number;
}

/** The permission book's view (agent and external requests). `approvalId` joins the owner-approvals pop-up. */
export interface PermissionView {
  id: string;
  kind: "agent" | "external";
  agentId?: string;
  source?: string;
  name?: string;
  title: string;
  description?: string;
  details?: Record<string, unknown>;
  status: "pending" | "allowed" | "denied" | "expired";
  createdAt: string;
  expiresAt: string;
  resolution?: { behavior: "allow" | "deny"; by: string; at: string; message?: string };
  approvalId?: string;
}

export interface WorkspaceRecord {
  workspaceId: string;
  projectId: string;
  cwd: string;
  name: string;
  displayName?: string;
  projectDisplayName?: string;
  isolation: "local" | "worktree";
  branch?: string;
  background?: boolean;
  archivedAt?: string | null;
}

export interface ProjectRecord {
  projectId: string;
  rootPath: string;
  name: string;
}

export interface ModelInfo {
  id: string;
  label: string;
  isDefault?: boolean;
  isSelectable?: boolean;
}

export interface ProviderInfo {
  id: string;
  label: string;
  defaultModeId?: string | null;
  modes: { id: string; label: string; description?: string }[];
  models: ModelInfo[];
}

export interface DiffLine {
  type: "add" | "remove" | "context" | "header";
  content: string;
}
export interface DiffHunk {
  oldStart: number;
  oldCount: number;
  newStart: number;
  newCount: number;
  lines: DiffLine[];
}
export interface DiffFile {
  path: string;
  oldPath?: string;
  isNew: boolean;
  isDeleted: boolean;
  additions: number;
  deletions: number;
  hunks: DiffHunk[];
  status: "ok" | "binary" | "too_large" | string;
}
export interface DiffResult {
  cwd: string;
  files: DiffFile[];
  diffTooLarge: boolean;
  error: string | null;
}
export type DiffMode = "all" | "uncommitted" | "base";

export interface FileEntry {
  name: string;
  path: string;
  kind: "file" | "directory" | string;
  size: number;
  modifiedAt: string;
}
export interface FileContent {
  path: string;
  kind: string;
  encoding: string;
  content?: string;
  mimeType?: string;
  size: number;
  modifiedAt: string;
}

export interface SearchMatch {
  field: string;
  seq?: number;
  kind?: string;
  snippet: string;
}
export interface SearchResult {
  agentId: string;
  title: string | null;
  provider: string;
  status: AgentStatus;
  cwd: string;
  updatedAt: string;
  archivedAt?: string | null;
  score?: number;
  matches: SearchMatch[];
}

export interface PromptEntry {
  seq: number;
  timestamp: string;
  preview: string;
}

export interface ScheduleRun {
  id: string;
  scheduledFor: string;
  startedAt: string;
  endedAt: string | null;
  status: "running" | "succeeded" | "failed";
  agentId: string | null;
  error?: string | null;
}
export interface Schedule {
  id: string;
  name: string | null;
  prompt: string;
  cadence: { type: "cron"; expression: string; timezone?: string } | { type: "every"; everyMs: number };
  target: { type: "agent"; agentId: string } | { type: "new-agent"; config: { provider: string; cwd?: string; title?: string } };
  status: "active" | "paused" | "completed";
  nextRunAt: string | null;
  lastRunAt: string | null;
  runs?: ScheduleRun[];
}

export interface UsageProvider {
  provider: string;
  agents: number;
  running: number;
  inputTokens: number;
  cachedInputTokens: number;
  outputTokens: number;
  totalCostUsd: number;
}
export interface UsageReport {
  providers: UsageProvider[];
  schedules?: { active: number; paused: number; completed: number };
  since?: string;
}

export interface TerminalPort {
  port: number;
  host?: string;
}
export interface Terminal {
  id: string;
  name: string;
  title?: string;
  cwd: string;
  workspaceId: string;
  agentId?: string | null;
  owner?: string;
  scriptName?: string | null;
  rows: number;
  cols: number;
  status: "running" | "exited";
  exitCode?: number | null;
  activity?: "running" | "idle" | "needs-input" | null;
  ports?: TerminalPort[];
}

export interface Script {
  scriptName: string;
  type: "script" | "service";
  port?: number | null;
  lifecycle: "running" | "stopped";
  health?: "healthy" | "unhealthy" | null;
  exitCode?: number | null;
  terminalId?: string | null;
  command?: string;
  servicePath?: string | null;
}
export interface Service {
  name: string;
  kind: "script" | "detected";
  scriptName?: string | null;
  port: number;
  health?: string | null;
  path: string;
}

/** One worktree on the board. `state` is agentd's: open, merge_queued, merging, merged, stale. */
export interface BoardWorkspace {
  workspaceId: string;
  project?: string | null;
  branch?: string | null;
  name?: string | null;
  cwd: string;
  state: string;
  ahead?: number | null;
  behind?: number | null;
  dirty?: boolean;
  idleDays?: number | null;
  /** `lagent top`: what runs inside the worktree right now. */
  procs?: number;
  rssMb?: number;
  commands?: Record<string, number>;
  task?: { title?: string; session?: string; owner?: string } | null;
}

export interface BoardJob {
  jobId: string;
  kind: "merge" | "deploy";
  state: "queued" | "running" | "succeeded" | "failed" | "cancelled";
  repo?: string;
  branch?: string | null;
  runtime?: string | null;
  createdAt: string;
  startedAt?: string | null;
  finishedAt?: string | null;
  requestedBy?: string | null;
  outcome?: { code?: string; message?: string; ok?: boolean } | null;
}

export interface BoardDevice {
  device: string;
  about?: string;
  lease?: { holder?: string; purpose?: string; until?: string; expiresAt?: string } | null;
}

export interface BoardDeploy {
  runtime: string;
  commit: string;
  recorded: string;
  by?: string;
  note?: string;
}

/** `GET /v1/agents/pipeline/board`, what `lagent board` prints. */
export interface PipelineBoard {
  workspaces?: BoardWorkspace[];
  queue?: BoardJob[];
  recentJobs?: BoardJob[];
  leases?: BoardDevice[];
  deploys?: BoardDeploy[];
  mainCheckouts?: { path: string; dirtyFiles?: number; procs?: number }[];
  unmanaged?: { path: string }[];
}

/** One body of the shared stream (`GET /v1/agents/stream`). */
export type BusEvent =
  | { type: "agent_state"; agent: AgentSnapshot }
  | { type: "agent_stream"; agentId: string; event: { type: string; item?: TimelineItem; [k: string]: unknown }; seq?: number; epoch: string; timestamp: string }
  | { type: "agent_deleted"; agentId: string }
  | { type: "permission"; permission: PermissionView }
  | { type: "workspace"; workspace: WorkspaceRecord }
  | { type: "terminal"; terminal: Terminal }
  | { type: "terminal_removed"; terminalId: string; workspaceId?: string }
  | { type: "script_status"; workspaceId: string; scripts: Script[] }
  | { type: "stream.reset" }
  | { type: string; [k: string]: unknown };

/** The route says "not deployed here": hide the feature instead of showing an error. */
export function notDeployed(e: unknown): boolean {
  return e instanceof ApiError && (e.status === 404 || e.status === 405 || e.status === 501);
}

function qs(params: Record<string, string | number | boolean | undefined>): string {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v !== undefined && v !== "") p.set(k, String(v));
  const s = p.toString();
  return s ? `?${s}` : "";
}

export interface CreateAgentBody {
  provider: string;
  model?: string;
  cwd: string;
  workspaceId?: string;
  prompt?: string;
  modeId?: string;
  title?: string;
}

export interface CreateWorkspaceBody {
  isolation: "local" | "worktree";
  projectId?: string;
  path?: string;
  title?: string;
  prompt?: string;
  provider?: string;
}

export const agentsApi = {
  items: (archived = false) => request<{ agents: AgentListItem[] }>("GET", `${B}/items${qs({ archived: archived || undefined })}`).then((r) => r.agents ?? []),
  snapshots: (archived = false) => request<{ agents: AgentSnapshot[] }>("GET", `${B}${qs({ archived: archived || undefined })}`).then((r) => r.agents ?? []),
  snapshot: (id: string) => request<AgentSnapshot>("GET", `${B}/${enc(id)}`),
  providers: () => request<ProviderInfo[]>("GET", `${B}/providers`),
  workspaces: () => request<{ workspaces: WorkspaceRecord[] }>("GET", `${B}/workspaces`).then((r) => r.workspaces ?? []),
  projects: () => request<{ projects: ProjectRecord[] }>("GET", `${B}/projects`).then((r) => r.projects ?? []),
  createWorkspace: (body: CreateWorkspaceBody) => request<{ workspace: WorkspaceRecord; setupSkippedReason?: string | null }>("POST", `${B}/workspaces`, { body, timeoutMs: 120_000 }),
  createAgent: (body: CreateAgentBody) => request<AgentSnapshot>("POST", B, { body, timeoutMs: 60_000 }),
  send: (id: string, prompt: string, steer: boolean) =>
    request<{ agentId: string; disposition: string }>("POST", `${B}/${enc(id)}/send`, { body: steer ? { prompt, activeTurnBehavior: "steer" } : { prompt }, timeoutMs: 60_000 }),
  stop: (id: string) => request<{ agentId: string; stopped: boolean }>("POST", `${B}/${enc(id)}/stop`),
  archive: (id: string) => request<{ agentId: string; archivedAt: string }>("POST", `${B}/${enc(id)}/archive`, { timeoutMs: 60_000 }),
  unarchive: (id: string) => request<AgentSnapshot>("POST", `${B}/${enc(id)}/unarchive`),
  rename: (id: string, title: string) => request<AgentSnapshot>("PATCH", `${B}/${enc(id)}`, { body: { title } }),
  detach: (id: string) => request<AgentSnapshot>("POST", `${B}/${enc(id)}/detach`),
  timeline: (id: string, afterSeq: number, limit = 500) => request<TimelinePage>("GET", `${B}/${enc(id)}/timeline${qs({ afterSeq, limit })}`),
  prompts: (id: string) => request<{ epoch: string; prompts: PromptEntry[] }>("GET", `${B}/${enc(id)}/prompts`).then((r) => r.prompts ?? []),

  permissions: (all = false) => request<{ permissions: PermissionView[] }>("GET", `${B}/permissions${qs({ all: all || undefined })}`).then((r) => r.permissions ?? []),
  /** An agent's own request: Paseo's PermissionResponse. */
  answerAgentPermission: (agentId: string, requestId: string, behavior: "allow" | "deny", message?: string) =>
    request<{ agentId: string; requestId: string; behavior: string }>("POST", `${B}/${enc(agentId)}/permissions/${enc(requestId)}`, {
      body: behavior === "deny" && message ? { behavior, message } : { behavior },
      timeoutMs: 30_000,
    }),
  /** An external request (the Infisical broker asking before it hands out a secret). */
  answerExternalPermission: (id: string, behavior: "allow" | "deny", message?: string) =>
    request<PermissionView>("POST", `${B}/permissions/${enc(id)}`, { body: message ? { behavior, message } : { behavior } }),

  diff: (workspaceId: string, mode: DiffMode, ignoreWhitespace: boolean) =>
    request<DiffResult>("GET", `${B}/workspaces/${enc(workspaceId)}/diff${qs({ mode, ignoreWhitespace: ignoreWhitespace || undefined })}`, { timeoutMs: 60_000 }),
  files: (workspaceId: string, path: string) =>
    request<{ path: string; entries: FileEntry[] }>("GET", `${B}/workspaces/${enc(workspaceId)}/files${qs({ path })}`),
  file: (workspaceId: string, path: string) =>
    request<FileContent>("GET", `${B}/workspaces/${enc(workspaceId)}/file${qs({ path, maxBytes: 262_144 })}`),
  /** A one-use download link (60 s) for a file. */
  downloadUrl: async (workspaceId: string, path: string): Promise<string> => {
    const r = await request<{ token: string; url: string }>("POST", `${B}/workspaces/${enc(workspaceId)}/download-token`, { body: { path } });
    return `${B}/files/download?token=${enc(r.token)}`;
  },

  search: (q: string, limit = 20) => request<{ query: string; results: SearchResult[] }>("GET", `${B}/search${qs({ q, limit })}`).then((r) => r.results ?? []),
  schedules: () => request<{ schedules: Schedule[] }>("GET", `${B}/schedules`).then((r) => r.schedules ?? []),
  pipelineBoard: () => request<PipelineBoard>("GET", `${B}/pipeline/board`),
  scheduleAction: (id: string, action: "pause" | "resume" | "run-once") => request<Schedule>("POST", `${B}/schedules/${enc(id)}/${action}`),
  usage: () => request<UsageReport>("GET", `${B}/usage`),

  terminals: () => request<{ terminals: Terminal[] }>("GET", `${B}/terminals`).then((r) => r.terminals ?? []),
  createTerminal: (workspaceId: string, name?: string) => request<Terminal>("POST", `${B}/terminals`, { body: name ? { workspaceId, name } : { workspaceId } }),
  killTerminal: (id: string) => request<{ terminalId: string; success: boolean }>("DELETE", `${B}/terminals/${enc(id)}`),
  terminalInput: (id: string, body: { data: string } | { keys: string | string[]; literal?: boolean }) =>
    request<{ terminalId: string; keysSent?: number }>("POST", `${B}/terminals/${enc(id)}/input`, { body }),
  terminalStreamUrl: (id: string, offset?: number) => `${B}/terminals/${enc(id)}/stream${qs({ offset })}`,
  scripts: (workspaceId: string) => request<{ scripts: Script[] }>("GET", `${B}/workspaces/${enc(workspaceId)}/scripts`).then((r) => r.scripts ?? []),
  services: (workspaceId: string) => request<{ services: Service[] }>("GET", `${B}/workspaces/${enc(workspaceId)}/services`).then((r) => r.services ?? []),
  scriptAction: (workspaceId: string, name: string, action: "start" | "stop") =>
    request<Script>("POST", `${B}/workspaces/${enc(workspaceId)}/scripts/${enc(name)}/${action}`, { timeoutMs: 60_000 }),
};

export const AGENTS_STREAM_URL = `${B}/stream`;
