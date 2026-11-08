import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import type { Group, TasksView, TaskView } from "../api/tasks";
import { setLocale } from "../i18n";
import { readJSON } from "../lib/storage";
import { OUTBOX_KEY, resetTasksState } from "../state/tasks";
import { stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
import TasksPage from "./Tasks";

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

describe("Tasks", () => {
  it("shows Today like Todoist: sidebar, overdue with reschedule, date header, chips and goal", async () => {
    server();
    renderTasks();
    expect(await screen.findByText("15 min shadowing")).toBeInTheDocument();
    const nav = screen.getByRole("navigation", { name: "Tasks" });
    for (const name of ["Add task", "Search", "Inbox", "Today", "Upcoming", "Filters & Labels", "Completed", "My Projects", "Lingue"]) {
      expect(within(nav).getAllByText(name).length).toBeGreaterThan(0);
    }
    expect(within(nav).getByRole("link", { name: /Inbox/ })).toHaveTextContent("4");
    const overdue = screen.getByRole("region", { name: "Overdue" });
    expect(within(overdue).getByText("10 min anki/memrise")).toBeInTheDocument();
    expect(within(overdue).getByRole("button", { name: "Reschedule" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: /Today · Thursday/ })).toBeInTheDocument();
    const anki = within(overdue).getByRole("button", { name: /Open task: 10 min anki/ });
    expect(anki.querySelector(".task-check--p1")).not.toBeNull();
    expect(anki.querySelector(".due-chip--overdue")).not.toBeNull();
    expect(screen.getByText("1/5 today")).toBeInTheDocument();
    expect(screen.getByText("2 tasks")).toBeInTheDocument();
    expect(screen.getByText(/Synced with Todoist/)).toBeInTheDocument();
  });

  it("completes with an op_id and no revision, then undoes from the toast", async () => {
    const calls = server();
    renderTasks();
    await screen.findByText("15 min shadowing");
    fireEvent.click(screen.getAllByRole("button", { name: "Complete" })[1] as HTMLElement);
    await waitFor(() => expect(commands(calls)).toHaveLength(1));
    const sent = commands(calls)[0];
    expect(sent?.command).toEqual({ type: "complete", task_id: "a1" });
    expect(sent?.op_id).toMatch(/^[0-9a-f-]{36}$/);
    expect(sent?.expected_revision).toBeUndefined();
    expect(sent?.view).toEqual({ kind: "today" });
    expect(calls.find((c) => c.method === "POST")?.headers["x-secretary-csrf"]).toBe("1");
    expect(await screen.findByText("1 task completed")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(commands(calls)[1]?.command).toEqual({ type: "uncomplete", task_id: "a1" }));
  });

  it("reschedules the overdue group with the date picker", async () => {
    const calls = server();
    renderTasks();
    fireEvent.click(await screen.findByRole("button", { name: "Reschedule" }));
    const dialog = screen.getByRole("dialog", { name: "Reschedule overdue tasks" });
    for (const label of ["Today", "Tomorrow", "This weekend", "Next week", "Pick a date"]) {
      expect(within(dialog).getAllByText(label).length).toBeGreaterThan(0);
    }
    fireEvent.click(within(dialog).getByText("Tomorrow"));
    await waitFor(() => expect(commands(calls)[0]?.command).toEqual({ type: "reschedule_overdue", date: "2026-10-23" }));
    expect(await screen.findByText("1 task rescheduled")).toBeInTheDocument();
  });

  it("quick add on q: live chips from the engine, Enter adds (due today from Today), Esc closes", async () => {
    const calls = server();
    renderTasks();
    await screen.findByText("15 min shadowing");
    fireEvent.keyDown(document, { key: "q" });
    const dialog = await screen.findByRole("dialog", { name: "Quick add" });
    const input = within(dialog).getByLabelText("Task name");
    fireEvent.input(input, { target: { value: "Ripasso domani alle 9 #Lingue @lingue p1" } });
    expect(await within(dialog).findByText("Lingue")).toBeInTheDocument();
    expect(within(dialog).getByText(/Tomorrow 09:00/)).toBeInTheDocument();
    expect(within(dialog).getByText("Priority 1")).toBeInTheDocument();
    fireEvent.submit(input.closest("form") as HTMLFormElement);
    await waitFor(() => expect(commands(calls).some((c) => c.command.type === "quick_add")).toBe(true));
    const add = commands(calls).find((c) => c.command.type === "quick_add");
    expect(add?.command).toMatchObject({ text: "Ripasso domani alle 9 #Lingue @lingue p1", default_due: "today" });
    expect(await screen.findByText("Task added")).toBeInTheDocument();
    fireEvent.keyDown(within(dialog).getByLabelText("Task name"), { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Quick add" })).not.toBeInTheDocument());
  });

  it("keeps commands offline and sends them when the connection is back", async () => {
    let down = true;
    const calls = server((c) => {
      if (c.method === "POST" && down) throw new TypeError("Failed to fetch");
      return undefined;
    });
    renderTasks();
    await screen.findByText("15 min shadowing");
    fireEvent.click(screen.getAllByRole("button", { name: "Complete" })[1] as HTMLElement);
    expect(await screen.findByText("Offline: 1 change will be sent when you're back online")).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText("15 min shadowing")).not.toBeInTheDocument());
    const queued = readJSON("local", OUTBOX_KEY) as { op_id: string; command: unknown }[];
    expect(queued[0]?.command).toEqual({ type: "complete", task_id: "a1" });
    down = false;
    window.dispatchEvent(new Event("online"));
    await waitFor(() => expect(readJSON("local", OUTBOX_KEY)).toEqual([]));
    const sent = commands(calls).filter((c) => c.command.type === "complete");
    expect(sent.at(-1)?.op_id).toBe(queued[0]?.op_id);
  });

  it("searches from the sidebar and opens a task's detail", async () => {
    const calls = server();
    renderTasks("/tasks/search");
    const box = await screen.findByLabelText("Search tasks");
    fireEvent.input(box, { target: { value: "anki" } });
    await waitFor(() => expect(calls.some((c) => c.url.includes("view=search") && c.url.includes("query=anki"))).toBe(true));
    fireEvent.click(await screen.findByRole("button", { name: /Open task: 10 min anki/ }));
    const detail = await screen.findByRole("dialog", { name: "10 min anki/memrise" });
    expect(within(detail).getByRole("radio", { name: "P1" })).toHaveAttribute("aria-checked", "true");
    fireEvent.click(within(detail).getByRole("radio", { name: "P2" }));
    await waitFor(() => expect(commands(calls).at(-1)?.command).toEqual({ type: "update", task_id: "a2", priority: 2 }));
  });

  it("speaks Italian and points to Links when Todoist is not connected", async () => {
    setLocale("it");
    server((c) => {
      if (c.method === "GET" && c.url.startsWith("/v1/tasks/state")) {
        return { body: { ok: true, view: state(todayGroups(), { sync: { status: "disconnected", connected: false, pending: 0 } }) } };
      }
      return undefined;
    });
    renderTasks();
    expect(await screen.findByText("In ritardo")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Riprogramma" })).toBeInTheDocument();
    expect(screen.getAllByText("Oggi").length).toBeGreaterThan(0);
    expect(screen.getByText(/Todoist non è collegato/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Collega in Link" })).toHaveAttribute("href", "/links");
  });
});
