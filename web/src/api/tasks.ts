import { request } from "./client";

// Tasks module wire types, per docs/server-api.md ("Tasks"). The engine
// (crates/secretary-tasks) computes every list, count, date and the quick
// add parse; this file only moves JSON.
//
// GET  /v1/tasks/state?view=&id=&query=&days=&timezone=  -> {ok, view}
// POST /v1/tasks/command  {op_id, timezone, command}      -> {ok, replayed, view, task_id?, parsed?, queued?}
//                         200 ok · 409 conflict · 422 invalid_command/op_id_reused · 500 storage_error
// POST /v1/tasks/sync                                      -> {ok, report} · 409 todoist_not_connected/_reconnect · 429

export type ViewKind = "inbox" | "today" | "upcoming" | "project" | "label" | "filter" | "search" | "completed";

export interface ViewQuery {
  kind: ViewKind;
  id?: string;
  query?: string;
  days?: number;
}

export interface DueView {
  date: string;
  time?: string;
  string: string;
  is_recurring: boolean;
  rel_days: number;
  overdue: boolean;
  provisional?: boolean;
}

export interface TaskView {
  id: string;
  content: string;
  description: string;
  project_id: string;
  project_name: string;
  project_color: string;
  section_id?: string;
  section_name?: string;
  parent_id?: string;
  parent_content?: string;
  /** 1 (urgent, red) … 4 (none). */
  priority: number;
  due?: DueView;
  deadline?: string;
  labels: string[];
  checked: boolean;
  subtasks: number;
  subtasks_done: number;
  pending: boolean;
  duration_minutes?: number;
  children?: TaskView[];
  completed_ms?: number;
}

export interface Group {
  key: string;
  kind: "overdue" | "date" | "section" | "none" | "completed";
  title: string;
  date?: string;
  rel_days?: number;
  tasks: TaskView[];
}

export interface ListView {
  kind: ViewKind | string;
  id?: string;
  title: string;
  query?: string;
  groups: Group[];
  count: number;
  warning?: string;
}

export interface ProjectView {
  id: string;
  name: string;
  color: string;
  parent_id?: string;
  inbox: boolean;
  favorite: boolean;
  count: number;
  sections: { id: string; name: string }[];
}

export interface NamedCount {
  id: string;
  name: string;
  color: string;
  favorite: boolean;
  count: number;
  query?: string;
}

export type SyncStatus = "disconnected" | "ok" | "syncing" | "error" | "rate_limited" | "reconnect";

export interface Conflict {
  id: string;
  task_id?: string;
  content: string;
  kind: "changed_remotely" | "deleted_remotely" | "completed_remotely" | "rejected" | "recurrence_lost" | string;
  message: string;
  at_ms: number;
}

export interface TasksView {
  revision: number;
  today: string;
  timezone: string;
  counts: { inbox: number; today: number; overdue: number; upcoming: number };
  goal: { daily_goal: number; completed_today: number; reached: boolean };
  inbox_id?: string;
  projects: ProjectView[];
  labels: NamedCount[];
  filters: NamedCount[];
  sync: {
    status: SyncStatus;
    connected: boolean;
    account?: string;
    last_sync_ms?: number;
    pending: number;
    error?: string;
    retry_after_ms?: number;
  };
  conflicts: Conflict[];
  undo: string[];
  list: ListView;
}

export interface Span {
  start: number;
  end: number;
  kind: "date" | "project" | "section" | "label" | "priority" | "description" | "deadline";
}

export interface Parsed {
  content: string;
  description?: string;
  due_string?: string;
  due_date?: string;
  is_recurring: boolean;
  priority?: number;
  labels: string[];
  project?: string;
  section?: string;
  lang: string;
  spans: Span[];
}

export type TaskCommand =
  | { type: "quick_add"; text: string; lang?: string; project_id?: string; section_id?: string; default_due?: string }
  | {
      type: "add";
      content: string;
      description?: string;
      project_id?: string;
      section_id?: string;
      parent_id?: string;
      due_string?: string;
      due_date?: string;
      priority?: number;
      labels?: string[];
    }
  | { type: "complete"; task_id: string }
  | { type: "uncomplete"; task_id: string }
  | {
      type: "update";
      task_id: string;
      content?: string;
      description?: string;
      priority?: number;
      labels?: string[];
      due_string?: string;
      due_date?: string;
      clear_due?: boolean;
    }
  | { type: "reschedule"; task_id: string; date: string }
  | { type: "reschedule_overdue"; date?: string }
  | { type: "delete"; task_id: string }
  | { type: "undo_delete"; task_id: string }
  | { type: "move"; task_id: string; project_id?: string; section_id?: string; parent_id?: string }
  | { type: "set_daily_goal"; goal: number }
  | { type: "dismiss_conflict"; conflict_id: string };

export interface TasksResponse {
  ok: boolean;
  replayed?: boolean;
  code?: string;
  error?: string;
  view?: TasksView;
  task_id?: string;
  parsed?: Parsed;
  queued?: boolean;
}

function qs(q: ViewQuery, timezone: string): string {
  const p = new URLSearchParams({ view: q.kind, timezone });
  if (q.id) p.set("id", q.id);
  if (q.query) p.set("query", q.query);
  if (q.days) p.set("days", String(q.days));
  return p.toString();
}

export const tasksApi = {
  state: (q: ViewQuery, timezone: string) => request<TasksResponse>("GET", `/v1/tasks/state?${qs(q, timezone)}`),
  /** `view`: the list the answer shows (the open one), so no second request is needed. */
  command: (op_id: string, timezone: string, command: TaskCommand, view?: ViewQuery) =>
    request<TasksResponse>("POST", "/v1/tasks/command", { body: { op_id, timezone, command, view } }),
  parse: (text: string, timezone: string) =>
    request<TasksResponse>("POST", "/v1/tasks/command", {
      body: { timezone, command: { type: "parse_quick_add", text } },
    }),
  sync: () => request<{ ok: boolean; report?: unknown }>("POST", "/v1/tasks/sync", { body: {}, timeoutMs: 45_000 }),
};
