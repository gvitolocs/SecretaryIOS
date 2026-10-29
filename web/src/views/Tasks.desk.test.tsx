import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Group, TasksView, TaskView } from "../api/tasks";
import { setLocale } from "../i18n";
import { resetTasksState } from "../state/tasks";
import { deferred, stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
import TasksPage from "./Tasks";
import { stubWide } from "../test/wide";

const TODAY = "2026-10-22";

function task(over: Partial<TaskView>): TaskView {
  return {
    id: "t",
    content: "task",
    description: "",
    project_id: "inbox1",
    project_name: "Inbox",
    project_color: "charcoal",
    priority: 4,
    labels: [],
    checked: false,
    subtasks: 0,
    subtasks_done: 0,
    pending: false,
    ...over,
  };
}

const SHADOWING = task({
  id: "a1",
  content: "15 min shadowing",
  due: { date: TODAY, string: "every day", is_recurring: true, rel_days: 0, overdue: false },
  labels: ["lingue"],
});
const ANKI = task({
  id: "a2",
  content: "10 min anki/memrise",
  priority: 1,
  due: { date: "2026-10-20", string: "ogni giorno", is_recurring: true, rel_days: -2, overdue: true },
});

function state(groups: Group[], over: Partial<TasksView> = {}): TasksView {
  return {
    revision: 7,
    today: TODAY,
    timezone: "Europe/Copenhagen",
    counts: { inbox: 4, today: 2, overdue: 1, upcoming: 1 },
    goal: { daily_goal: 5, completed_today: 1, reached: false },
    inbox_id: "inbox1",
    projects: [
      { id: "inbox1", name: "Inbox", color: "charcoal", inbox: true, favorite: false, count: 4, sections: [] },
      { id: "lang", name: "Lingue", color: "blue", inbox: false, favorite: false, count: 3, sections: [{ id: "s1", name: "Speaking" }] },
    ],
    labels: [{ id: "l1", name: "lingue", color: "green", favorite: false, count: 1 }],
    filters: [{ id: "f1", name: "Urgent", color: "red", favorite: false, count: 0, query: "p1 & today" }],
    sync: { status: "ok", connected: true, account: "owner@example.com", last_sync_ms: Date.now() - 60_000, pending: 0 },
    conflicts: [],
    undo: [],
    list: { kind: "today", title: "Today", groups, count: 2 },
    ...over,
  };
}

const todayGroups = (): Group[] => [
  { key: "overdue", kind: "overdue", title: "Overdue", tasks: [ANKI] },
  { key: TODAY, kind: "date", title: "", date: TODAY, rel_days: 0, tasks: [SHADOWING] },
];

function server(over: (c: Call) => { status?: number; body?: unknown } | undefined = () => undefined) {
  return stubFetch((c) => {
    const o = over(c);
    if (o) return o;
    const url = new URL(c.url, "http://x");
    if (c.method === "GET" && url.pathname === "/v1/tasks/state") {
      const kind = url.searchParams.get("view") ?? "today";
      if (kind === "search") {
        const q = (url.searchParams.get("query") ?? "").toLowerCase();
        const hits = [ANKI, SHADOWING].filter((x) => x.content.toLowerCase().includes(q));
        return { body: { ok: true, view: state([{ key: "none", kind: "none", title: "", tasks: hits }], { list: { kind: "search", title: q, groups: [{ key: "none", kind: "none", title: "", tasks: hits }], count: hits.length } }) } };
      }
      return { body: { ok: true, view: state(todayGroups()) } };
    }
    if (c.method === "POST" && url.pathname === "/v1/tasks/command") {
      const body = c.body as { command: { type: string; text?: string } };
      if (body.command.type === "parse_quick_add") {
        return {
          body: {
            ok: true,
            parsed: { content: "Ripasso", due_string: "domani alle 9", due_date: "2026-10-23T09:00", is_recurring: false, priority: 1, labels: ["lingue"], project: "Lingue", lang: "it", spans: [] },
          },
        };
      }
      return { body: { ok: true, replayed: false, queued: true, task_id: "tmp-1", view: state(todayGroups()) } };
    }
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
}

const renderTasks = (path = "/tasks") => renderAt(path, () => <Route path="/tasks/:section?/:id?" component={TasksPage} />);
const commands = (calls: Call[]) =>
  calls.filter((c) => c.method === "POST" && c.url === "/v1/tasks/command").map((c) => c.body as { op_id?: string; command: { type: string } & Record<string, unknown>; view?: unknown; expected_revision?: number });

afterEach(() => {
  resetTasksState();
  setLocale(null);
});

describe("Tasks desktop page system", () => {
  beforeEach(() => stubWide());
  afterEach(() => vi.unstubAllGlobals());

  async function header(current: string) {
    expect(await screen.findByRole("heading", { level: 1, name: "Tasks" })).toBeInTheDocument();
    const nav = screen.getByRole("navigation", { name: "Tasks" });
    expect(within(nav).getByRole("link", { name: current })).toHaveAttribute("aria-current", "page");
    expect(nav.querySelectorAll('[aria-current="page"]')).toHaveLength(1);
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(1);
    expect(document.querySelector(".pg-head .pg-cta")).toHaveTextContent("Add task");
    expect(document.querySelector(".tasks-sidebar, .tasks-head, .tasks-fab, .view-title")).toBeNull();
    expect(document.querySelector(".pg-grid--rail")).toBeInTheDocument();
  }

  it("keeps the header and sections visible while loading", async () => {
    const response = deferred<{ body: unknown }>();
    stubFetch(() => response.promise);
    renderTasks();
    await header("Today");
    expect(screen.getByRole("status")).toHaveClass("pg-loading");
    response.resolve({ body: { ok: true, view: state(todayGroups()) } });
    expect(await screen.findByRole("region", { name: "Overdue" })).toHaveClass("pg-card--list");
  });

  it("groups real tasks into list cards, preserving metadata, children, projects and sync", async () => {
    const parent = task({ ...ANKI, description: "Practice daily", subtasks: 1, children: [task({ id: "child", content: "Read aloud" })], labels: ["lingue"], deadline: "2026-10-23", parent_content: "Study" });
    server((c) => c.method === "GET" ? { body: { ok: true, view: state([{ key: "overdue", kind: "overdue", title: "Overdue", tasks: [parent] }]) } } : undefined);
    renderTasks();
    await header("Today");
    const group = await screen.findByRole("region", { name: "Overdue" });
    expect(group).toHaveClass("pg-card--list");
    expect(within(group).getByRole("list")).toHaveClass("pg-rows");
    expect(within(group).getAllByRole("listitem")).toHaveLength(2);
    for (const value of ["Practice daily", "Read aloud", "lingue", "Priority 1", "Study", "0/1"]) {
      expect(within(group).getByText(value)).toBeInTheDocument();
    }
    expect(within(group).getByText(/Deadline/)).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "My Projects" })).toHaveClass("pg-card");
    expect(screen.getByRole("link", { name: "Lingue" })).toHaveAttribute("href", "/tasks/project/lang");
    expect(screen.getByText("1/5 today")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Sync now" })).toHaveClass("pg-btn");
    expect(screen.getByText(/Synced with Todoist/)).toBeInTheDocument();
  });

  it("shows a friendly empty Inbox beneath the same header", async () => {
    server((c) => c.method === "GET" ? { body: { ok: true, view: state([], { list: { kind: "inbox", title: "Inbox", groups: [], count: 0 } }) } } : undefined);
    renderTasks("/tasks/inbox");
    await header("Inbox");
    expect(await screen.findByText("Your inbox is clear")).toHaveClass("pg-empty-text");
    expect(document.querySelector(".tasks-empty")).toBeNull();
  });

  it("shows a proxy failure in ErrorState and retries", async () => {
    let fail = true;
    const calls = server((c) => c.method === "GET" && fail ? { status: 502, body: { error: { code: "proxy", message: "Tasks proxy unavailable" } } } : undefined);
    renderTasks("/tasks/upcoming");
    await header("Upcoming");
    expect(await screen.findByRole("alert")).toHaveClass("pg-error");
    expect(screen.getByText("Tasks proxy unavailable")).toBeInTheDocument();
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByText("15 min shadowing");
    expect(calls.filter((c) => c.method === "GET")).toHaveLength(2);
  });

  it("renders filters and labels as linked list cards", async () => {
    server();
    renderTasks("/tasks/filters");
    await header("Filters & Labels");
    const filters = await screen.findByRole("region", { name: "Filters" });
    expect(filters).toHaveClass("pg-card--list");
    expect(within(filters).getByRole("link", { name: "Urgent" })).toHaveAttribute("href", "/tasks/filter/f1");
    const labels = screen.getByRole("region", { name: "Labels" });
    expect(within(labels).getByRole("list")).toHaveClass("pg-rows");
    expect(within(labels).getByRole("link", { name: "lingue" })).toHaveAttribute("href", "/tasks/label/lingue");
    expect(document.querySelector(".fl-page")).toBeNull();
  });

  it("adds from the header Sheet with today's default and restores focus", async () => {
    const calls = server();
    renderTasks();
    await screen.findByText("15 min shadowing");
    const add = document.querySelector<HTMLButtonElement>(".pg-cta")!;
    add.focus();
    fireEvent.click(add);
    const dialog = await screen.findByRole("dialog", { name: "Quick add" });
    expect(dialog).toHaveClass("pg-sheet");
    const input = within(dialog).getByLabelText("Task name");
    await waitFor(() => expect(input).toHaveFocus());
    fireEvent.input(input, { target: { value: "Practice" } });
    fireEvent.submit(input.closest("form")!);
    await waitFor(() => expect(commands(calls).some((c) => c.command.type === "quick_add" && c.command.default_due === "today")).toBe(true));
    fireEvent.keyDown(input, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(add).toHaveFocus();
  });

  it("retains completion, scheduling and the existing task detail dialog", async () => {
    const calls = server();
    renderTasks();
    const overdue = await screen.findByRole("region", { name: "Overdue" });
    fireEvent.click(within(overdue).getByRole("button", { name: "Complete" }));
    await waitFor(() => expect(commands(calls)[0]?.command).toEqual({ type: "complete", task_id: "a2" }));
    fireEvent.click(await screen.findByRole("button", { name: "Undo" }));
    await waitFor(() => expect(commands(calls).at(-1)?.command.type).toBe("uncomplete"));
    const current = await screen.findByRole("region", { name: "Overdue" });
    fireEvent.click(within(current).getByRole("button", { name: "Date" }));
    const picker = screen.getByRole("dialog");
    fireEvent.click(within(picker).getByText("Tomorrow"));
    await waitFor(() => expect(commands(calls).at(-1)?.command).toEqual({ type: "reschedule", task_id: "a2", date: "2026-10-23" }));
    fireEvent.click(within(screen.getByRole("region", { name: "Overdue" })).getByRole("button", { name: /Open task: 10 min anki/ }));
    expect(await screen.findByRole("dialog", { name: "10 min anki/memrise" })).toBeInTheDocument();
  });
});
