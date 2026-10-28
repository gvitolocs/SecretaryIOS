// What an agent's chat shows, from the timeline rows agentd stores. Providers stream many rows per visible item
// (assistant text in chunks, a tool call updated as it runs), so the rows are collapsed the way Paseo's
// timeline-projection does (crates/lucina-agents/src/projection.rs):
//   1. rows with the same tool call id fold into the first one (a later update wins, a known detail beats `unknown`);
//   2. adjacent assistant_message chunks of one turn concatenate, unless a chunk names a different messageId;
//   3. adjacent reasoning chunks of one turn concatenate.

import type { AgentListItem, AgentSnapshot, Bucket, TimelineItem, TimelineRow, ToolCallDetail } from "../api/agents";

export interface Entry {
  /** Stable key: the first row's seq. */
  key: number;
  seqStart: number;
  seqEnd: number;
  timestamp: string;
  turnId: string | null;
  item: TimelineItem;
}

function mergeTool(old: TimelineItem, next: TimelineItem): TimelineItem {
  const unknown = (d?: ToolCallDetail) => !d || d.type === "unknown";
  const detail = unknown(next.detail) && !unknown(old.detail) ? old.detail : next.detail;
  const metadata = old.metadata || next.metadata ? { ...(old.metadata ?? {}), ...(next.metadata ?? {}) } : undefined;
  return { ...next, detail, ...(metadata ? { metadata } : {}), agentMessage: next.agentMessage ?? old.agentMessage };
}

export function projectTimeline(rows: readonly TimelineRow[]): Entry[] {
  const out: Entry[] = [];
  const byCall = new Map<string, number>();
  for (const row of rows) {
    const item = row.item;
    const turn = row.turnId ?? null;
    if (item.type === "tool_call" && item.callId) {
      const at = byCall.get(`${turn ?? ""}/${item.callId}`);
      if (at !== undefined) {
        const e = out[at]!;
        out[at] = { ...e, seqEnd: row.seq, item: mergeTool(e.item, item) };
        continue;
      }
      byCall.set(`${turn ?? ""}/${item.callId}`, out.length);
    }
    const last = out[out.length - 1];
    if (last && last.turnId === turn && (item.type === "assistant_message" || item.type === "reasoning") && last.item.type === item.type) {
      const sameMessage = item.type === "reasoning" || !item.messageId || !last.item.messageId || item.messageId === last.item.messageId;
      if (sameMessage) {
        out[out.length - 1] = { ...last, seqEnd: row.seq, item: { ...last.item, text: (last.item.text ?? "") + (item.text ?? "") } };
        continue;
      }
    }
    out.push({ key: row.seq, seqStart: row.seq, seqEnd: row.seq, timestamp: row.timestamp, turnId: turn, item });
  }
  return out;
}

/** Merge rows into a timeline that is sorted by seq: duplicates are dropped. Returns the same array when nothing is new. */
export function mergeRows(have: readonly TimelineRow[], more: readonly TimelineRow[]): TimelineRow[] {
  const lastSeq = have.length ? have[have.length - 1]!.seq : -1;
  const fresh = more.filter((r) => r.seq > lastSeq).sort((a, b) => a.seq - b.seq);
  return fresh.length ? [...have, ...fresh] : (have as TimelineRow[]);
}

function str(v: unknown): string | undefined {
  return typeof v === "string" && v ? v : undefined;
}

export function baseName(path: string | undefined): string {
  if (!path) return "";
  const parts = path.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

function firstLine(s: string | undefined, max = 120): string {
  const line = (s ?? "").split("\n").find((l) => l.trim()) ?? "";
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

export interface ToolLine {
  /** The kind of the detail: shell, read, edit, write, search, fetch, sub_agent, plan, plain_text, … */
  kind: string;
  title: string;
  summary: string;
}

/** A short title and summary for a tool call row (Paseo's tool-call-display, reduced). */
export function toolLine(item: TimelineItem): ToolLine {
  const d = item.detail;
  const name = item.name ?? "tool";
  switch (d?.type) {
    case "shell":
      return { kind: "shell", title: "Shell", summary: firstLine(str(d.command)) };
    case "read":
      return { kind: "read", title: "Read", summary: baseName(str(d.filePath)) };
    case "edit":
      return { kind: "edit", title: "Edit", summary: baseName(str(d.filePath)) };
    case "write":
      return { kind: "write", title: "Write", summary: baseName(str(d.filePath)) };
    case "search":
      return { kind: "search", title: str(d.toolName) === "web_search" ? "Web search" : "Search", summary: firstLine(str(d.query)) };
    case "fetch":
      return { kind: "fetch", title: "Fetch", summary: str(d.url) ?? "" };
    case "sub_agent":
      return { kind: "sub_agent", title: "Subagent", summary: firstLine(str(d.description) ?? str(d.subAgentType)) };
    case "worktree_setup":
      return { kind: "worktree_setup", title: "Worktree setup", summary: str(d.branchName) ?? "" };
    case "plan":
      return { kind: "plan", title: "Plan", summary: firstLine(str(d.text)) };
    case "plain_text":
      return { kind: "plain_text", title: str(d.label) ?? name, summary: firstLine(str(d.text)) };
    default:
      return { kind: "unknown", title: name, summary: "" };
  }
}

/** The list bucket (crates/lucina-agents/src/bucket.rs) for an agent snapshot. */
export function bucketOf(s: Pick<AgentSnapshot, "status" | "pendingPermissions" | "requiresAttention" | "attentionReason">): Bucket {
  if ((s.pendingPermissions?.length ?? 0) > 0 || s.attentionReason === "permission") return "needs_input";
  if (s.status === "error" || s.attentionReason === "error") return "failed";
  if (s.status === "running") return "running";
  if (s.requiresAttention) return "attention";
  return "done";
}

export const BUCKET_ORDER: Record<Bucket, number> = { needs_input: 0, failed: 1, running: 2, attention: 3, done: 4 };

export function itemFromSnapshot(s: AgentSnapshot): AgentListItem {
  return {
    id: s.id,
    shortId: s.id.slice(0, 7),
    title: s.title,
    provider: s.provider,
    model: s.model,
    status: s.status,
    cwd: s.cwd,
    createdAt: s.createdAt,
    updatedAt: s.updatedAt,
    lastUserMessageAt: s.lastUserMessageAt,
    archivedAt: s.archivedAt ?? null,
    requiresAttention: s.requiresAttention,
    attentionReason: s.attentionReason,
    attentionTimestamp: s.attentionTimestamp,
    labels: s.labels,
    bucket: bucketOf(s),
    external: s.external,
    host: s.host,
  };
}

export function agentTitle(a: { title: string | null; shortId?: string; id: string; cwd?: string }): string {
  if (a.title && a.title.trim()) return a.title;
  const dir = baseName(a.cwd);
  return dir ? dir : (a.shortId ?? a.id.slice(0, 7));
}

/** Terminal output as text: the ANSI escape sequences are dropped, \r returns to the line start, \b erases. */
export function ansiToText(input: string): string {
  const s = input
    .replace(/\r\n/g, "\n")
    // CSI (colours, cursor moves), OSC (titles; ended by BEL or ESC \), and lone two-char escapes.
    .replace(/\u001b\[[0-?]*[ -/]*[@-~]/g, "")
    .replace(/\u001b\][^\u0007\u001b]*(?:\u0007|\u001b\\)/g, "")
    .replace(/\u001b[()][A-Za-z0-9]/g, "")
    .replace(/\u001b[=>78]/g, "");
  const lines: string[] = [""];
  for (const ch of s) {
    const cur = lines.length - 1;
    if (ch === "\n") lines.push("");
    else if (ch === "\r") lines[cur] = "";
    else if (ch === "\b") lines[cur] = lines[cur]!.slice(0, -1);
    else if (ch === "\u0007" || (ch < " " && ch !== "\t")) continue;
    else lines[cur] += ch;
  }
  return lines.join("\n");
}

/** Which pending-permission body to show: the command for a shell request, else the input as JSON. */
export function permissionText(details: Record<string, unknown> | undefined, request?: { detail?: ToolCallDetail }): string {
  const d = request?.detail;
  if (d?.type === "shell" && str(d.command)) return str(d.command)!;
  if (!details) return "";
  const cmd = str(details.command);
  if (cmd) return cmd;
  try {
    return JSON.stringify(details, null, 2);
  } catch {
    return "";
  }
}
