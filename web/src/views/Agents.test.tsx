import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { agentsState, resetAgentsState, streamTiming } from "../state/agents";
import { installEventSource, type MockEventSource } from "../test/eventsource";
import { item, permission, row, serveAgents, snapshot, workspace, type Fake } from "../test/agentsFixtures";
import { renderAt } from "../test/router";
import AgentsView from "./Agents";

let ES: typeof MockEventSource;

function renderAgents(path = "/agents") {
  return renderAt(path, () => <Route path="/agents/*rest" component={AgentsView} />);
}

/** The shared stream: open it and push a frame. */
function stream() {
  const es = ES.latest();
  es.open();
  return {
    es,
    send: (body: unknown, id?: string) => es.emit("message", body, id),
  };
}

beforeEach(() => {
  ES = installEventSource();
});
afterEach(() => {
  resetAgentsState();
});

const AGENTS = [
  item("a1", { title: "Fix the build", bucket: "needs_input", status: "running" }),
  item("a2", { title: "Write docs", bucket: "running", status: "running" }),
  item("a3", { title: "Old chore", bucket: "done", status: "idle", updatedAt: "2026-10-27T10:00:00Z" }),
  item("a4", { title: "Broken run", bucket: "failed", status: "error" }),
];

describe("agent list", () => {
  it("shows every agent with its status, the ones that need you first", async () => {
    serveAgents({ items: AGENTS, absent: ["/search", "/schedules"] });
    renderAgents();
    const list = await screen.findByRole("list", { name: "Agents" });
    await waitFor(() => expect(within(list).getAllByRole("link")).toHaveLength(4));
    const links = within(list).getAllByRole("link");
    expect(links.map((l) => within(l).getByText(/Fix|Write|Old|Broken/).textContent)).toEqual(["Fix the build", "Broken run", "Write docs", "Old chore"]);
    expect(within(links[0]!).getByText("Needs permission")).toBeInTheDocument();
    expect(within(links[1]!).getByText("Error")).toBeInTheDocument();
    expect(within(links[2]!).getByText("Running")).toBeInTheDocument();
    expect(within(links[3]!).getByText("Idle")).toBeInTheDocument();
    expect(links[0]!.querySelector(".ag-ring")).toHaveAttribute("data-bucket", "needs_input");
  });

  it("says so when there are no agents", async () => {
    serveAgents({ absent: ["/search", "/schedules"] });
    renderAgents();
    expect(await screen.findByText("No agents yet. Start a new run.")).toBeInTheDocument();
  });

  it("fetches the archived ones when the switch is turned on", async () => {
    const calls = serveAgents({ items: AGENTS, archived: [item("z1", { title: "Shelved", archivedAt: "2026-10-01T00:00:00Z" })], absent: ["/search", "/schedules"] });
    renderAgents();
    await screen.findByText("Fix the build");
    fireEvent.click(screen.getByRole("checkbox", { name: "Show archived" }));
    expect(await screen.findByText("Shelved")).toBeInTheDocument();
    expect(screen.queryByText("Fix the build")).not.toBeInTheDocument();
    expect(calls.some((c) => c.url === "/v1/agents/items?archived=true")).toBe(true);
  });

  it("hides the search box when the route is not deployed", async () => {
    serveAgents({ items: AGENTS, absent: ["/schedules"] });
    renderAgents();
    await screen.findByText("Fix the build");
    await waitFor(() => expect(screen.queryByRole("searchbox")).not.toBeInTheDocument());
  });

  it("searches on the server when the route answers", async () => {
    const calls = serveAgents({
      items: AGENTS,
      absent: ["/schedules"],
      extra: (c) =>
        c.url.startsWith("/v1/agents/search")
          ? { body: { query: "x", results: c.url.includes("q=docs") ? [{ agentId: "a2", title: "Write docs", provider: "claude", status: "running", cwd: "/x", updatedAt: "2026-10-28T11:00:00Z", matches: [{ field: "timeline", snippet: "…the docs site…" }] }] : [] } }
          : undefined,
    });
    renderAgents();
    const box = await screen.findByRole("searchbox", { name: "Search agents" });
    fireEvent.input(box, { target: { value: "docs" } });
    expect(await screen.findByText("…the docs site…")).toBeInTheDocument();
    expect(screen.queryByText("Fix the build")).not.toBeInTheDocument();
    expect(calls.some((c) => c.url.startsWith("/v1/agents/search?q=docs"))).toBe(true);
  });

  it("hides schedules when the route is not deployed, shows the pending count", async () => {
    serveAgents({ items: AGENTS, permissions: [permission()], absent: ["/schedules", "/search"] });
    renderAgents();
    await screen.findByText("Fix the build");
    await waitFor(() => expect(screen.queryByRole("link", { name: "Schedules" })).not.toBeInTheDocument());
    expect(await screen.findByLabelText("1 waiting")).toBeInTheDocument();
  });
});

describe("the shared stream", () => {
  it("opens one EventSource and applies agent_state, agent_deleted and permission frames", async () => {
    serveAgents({ items: [AGENTS[2]!], absent: ["/search", "/schedules"] });
    renderAgents();
    await screen.findByText("Old chore");
    expect(ES.instances).toHaveLength(1);
    expect(ES.latest().url).toBe("/v1/agents/stream");
    const s = stream();
    s.send({ type: "agent_state", agent: snapshot("a9", { title: "Brand new", status: "running" }) }, "5");
    expect(await screen.findByText("Brand new")).toBeInTheDocument();
    s.send({ type: "agent_state", agent: snapshot("a9", { title: "Brand new", status: "idle", requiresAttention: true, attentionReason: "finished" }) }, "6");
    await waitFor(() => expect(screen.getByRole("link", { name: /Brand new/ })).toHaveAttribute("data-bucket", "attention"));
    s.send({ type: "permission", permission: permission({ agentId: "a9" }) }, "7");
    expect(await screen.findByLabelText("1 waiting")).toBeInTheDocument();
    s.send({ type: "agent_command", agentId: "a9", commandId: "c", command: {} }, "8");
    s.send({ type: "agent_deleted", agentId: "a9" }, "9");
    await waitFor(() => expect(screen.queryByText("Brand new")).not.toBeInTheDocument());
  });

  it("refetches everything on stream.reset and resumes from the id it carries", async () => {
    const calls = serveAgents({ items: [AGENTS[2]!], absent: ["/search", "/schedules"] });
    renderAgents();
    await screen.findByText("Old chore");
    const before = calls.filter((c) => c.url === "/v1/agents/items").length;
    const { es, send } = stream();
    send({ type: "agent_state", agent: snapshot("a3", { title: "Old chore" }) }, "100");
    es.emit("stream.reset", { type: "stream.reset" }, "777");
    await waitFor(() => expect(calls.filter((c) => c.url === "/v1/agents/items").length).toBe(before + 1));
    // The connection fails for good (HTTP error): a new EventSource resumes with ?lastEventId=.
    es.fail();
    await waitFor(() => expect(ES.instances).toHaveLength(2), { timeout: 5000 });
    expect(ES.latest().url).toBe("/v1/agents/stream?lastEventId=777");
  });

  it("keeps the connection when the browser retries by itself", async () => {
    serveAgents({ items: [AGENTS[2]!], absent: ["/search", "/schedules"] });
    renderAgents();
    await screen.findByText("Old chore");
    const { es } = stream();
    es.drop();
    expect(ES.instances).toHaveLength(1);
    expect(es.closed).toBe(false);
  });

  it("closes the stream when the page is left", async () => {
    serveAgents({ items: [], absent: ["/search", "/schedules"] });
    const view = renderAgents();
    await screen.findByText("No agents yet. Start a new run.");
    const es = ES.latest();
    view.unmount();
    expect(es.closed).toBe(true);
  });
});

describe("what the page says about the connection", () => {
  const timing = { ...streamTiming };
  const REST_DOWN = { status: 503, body: { error: { code: "agentd_unavailable", message: "lucina-agentd is not running" } } };
  const agentsRest = (url: string) => /\/v1\/agents\/(items|permissions|workspaces)/.test(url);

  afterEach(() => void Object.assign(streamTiming, timing));

  it("says nothing while the stream is still connecting", async () => {
    serveAgents({ items: [AGENTS[2]!], absent: ["/search", "/schedules"] });
    renderAgents();
    await screen.findByText("Old chore");
    expect(screen.queryByText("Reconnecting…")).not.toBeInTheDocument();
    stream();
    expect(screen.queryByText("Reconnecting…")).not.toBeInTheDocument();
  });

  it("says nothing about a short drop, like an agentd restart", async () => {
    streamTiming.quietMs = 60;
    serveAgents({ items: [AGENTS[2]!], absent: ["/search", "/schedules"] });
    renderAgents();
    await screen.findByText("Old chore");
    const { es } = stream();
    es.drop();
    expect(screen.queryByRole("status", { name: /Reconnecting/ })).not.toBeInTheDocument();
    es.open();
    await new Promise((r) => setTimeout(r, 120));
    expect(screen.queryByText("Reconnecting…")).not.toBeInTheDocument();
    expect(screen.queryByText("Live updates are paused. Refreshing every 15 seconds.")).not.toBeInTheDocument();
  });

  it("shows the real error when the backend is down, and clears it by itself", async () => {
    Object.assign(streamTiming, { quietMs: 10, pollMs: 20, retryMs: 20 });
    let down = false;
    serveAgents({ items: [AGENTS[2]!], absent: ["/search", "/schedules"], extra: (call) => (down && agentsRest(call.url) ? REST_DOWN : undefined) });
    renderAgents();
    await screen.findByText("Old chore");
    const { es } = stream();
    down = true;
    es.fail();
    expect(await screen.findByText("Lucina Agents isn't answering. Trying again…")).toHaveAttribute("role", "alert");
    expect(await screen.findByText("lucina-agentd is not running")).toBeInTheDocument();
    // What was on the list stays there, and "no agents yet" never appears.
    expect(screen.getByText("Old chore")).toBeInTheDocument();
    expect(screen.queryByText("No agents yet. Start a new run.")).not.toBeInTheDocument();
    // The daemon is back: the next retry connects and the page is clean again.
    down = false;
    await waitFor(() => expect(ES.instances.length).toBeGreaterThan(1));
    ES.latest().open();
    await waitFor(() => expect(screen.queryByText("Lucina Agents isn't answering. Trying again…")).not.toBeInTheDocument());
    await waitFor(() => expect(screen.queryByText("lucina-agentd is not running")).not.toBeInTheDocument());
  });

  it("polls instead of saying reconnecting while requests work and only the stream doesn't", async () => {
    Object.assign(streamTiming, { quietMs: 10, pollMs: 20 });
    const calls = serveAgents({ items: [AGENTS[2]!], absent: ["/search", "/schedules"] });
    renderAgents();
    await screen.findByText("Old chore");
    const { es } = stream();
    es.drop();
    expect(await screen.findByText("Live updates are paused. Refreshing every 15 seconds.")).toHaveAttribute("role", "status");
    expect(screen.queryByText("Reconnecting…")).not.toBeInTheDocument();
    const before = calls.filter((c) => c.url === "/v1/agents/items").length;
    await waitFor(() => expect(calls.filter((c) => c.url === "/v1/agents/items").length).toBeGreaterThan(before));
    es.open();
    await waitFor(() => expect(screen.queryByText("Live updates are paused. Refreshing every 15 seconds.")).not.toBeInTheDocument());
  });

  it("starts over a connection attempt that never answers", async () => {
    Object.assign(streamTiming, { quietMs: 10, pollMs: 20, stuckMs: 30 });
    serveAgents({ items: [AGENTS[2]!], absent: ["/search", "/schedules"] });
    renderAgents();
    await screen.findByText("Old chore");
    const first = ES.latest();
    await waitFor(() => expect(ES.instances.length).toBeGreaterThan(1));
    expect(first.closed).toBe(true);
  });

  it("opens a new stream right away when the page comes back after a long time hidden", async () => {
    streamTiming.wakeMs = 0;
    serveAgents({ items: [AGENTS[2]!], absent: ["/search", "/schedules"] });
    renderAgents();
    await screen.findByText("Old chore");
    const { es } = stream();
    const visibility = Object.getOwnPropertyDescriptor(document, "visibilityState");
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "hidden" });
    document.dispatchEvent(new Event("visibilitychange"));
    await new Promise((r) => setTimeout(r, 5));
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "visible" });
    document.dispatchEvent(new Event("visibilitychange"));
    if (visibility) Object.defineProperty(document, "visibilityState", visibility);
    else delete (document as unknown as { visibilityState?: string }).visibilityState;
    expect(es.closed).toBe(true);
    expect(ES.instances).toHaveLength(2);
  });
});

describe("pipeline", () => {
  const BOARD = {
    workspaces: [
      { workspaceId: "wks_1", project: "SecretaryIOS", branch: "old-idea", cwd: "/w/old-idea", state: "stale", ahead: 3, dirty: false, idleDays: 9, procs: 0, rssMb: 0, commands: {}, task: { title: "old-idea" } },
      { workspaceId: "wks_2", project: "LucinaAndroidOS", branch: "net-dhcp", cwd: "/w/net-dhcp", state: "merging", ahead: 2, dirty: false, idleDays: 0, procs: 5, rssMb: 33, commands: { bash: 3, docker: 1 }, task: { title: "DHCP on the stick", session: "local_ff6cd29a-3fc2-4afe" } },
      { workspaceId: "wks_3", project: "SecretaryIOS", branch: "pipeline-page", cwd: "/w/pipeline-page", state: "open", ahead: 1, dirty: true, idleDays: 0, procs: 0, rssMb: 0, commands: {}, task: null },
    ],
    queue: [
      { jobId: "job_1", kind: "merge", state: "running", repo: "/p/LucinaAndroidOS", branch: "net-dhcp", createdAt: "2026-11-01T15:00:00Z", startedAt: "2026-11-01T15:00:01Z", requestedBy: "nez@nez_opt" },
      { jobId: "job_2", kind: "deploy", state: "queued", repo: "/p/SecretaryIOS", runtime: "secretary-server", createdAt: "2026-11-01T15:01:00Z" },
    ],
    recentJobs: [
      { jobId: "job_0", kind: "merge", state: "failed", repo: "/p/SecretaryIOS", branch: "broken", createdAt: "2026-11-01T14:00:00Z", finishedAt: "2026-11-01T14:01:00Z", outcome: { code: "GATE_FAILED", ok: false } },
    ],
    leases: [
      { device: "iphone", lease: { holder: "nez@nez_opt", purpose: "owner test", expiresAt: "2026-11-01T16:00:00Z" } },
      { device: "stick", lease: null },
    ],
    deploys: [{ runtime: "web", commit: "f74841ed19b627ec", recorded: "2026-11-01T14:53:07Z", note: "the calendar" }],
    mainCheckouts: [],
    unmanaged: [{ path: "/w/hand-made" }],
  };

  it("shows workspaces by state, running jobs, load, leases and deploys", async () => {
    const calls = serveAgents({ absent: ["/search", "/schedules"], extra: (c) => (c.url === "/v1/agents/pipeline/board" ? { body: BOARD } : undefined) });
    renderAgents("/agents/pipeline");
    const jobs = await screen.findByRole("list", { name: "Merges and deploys" });
    expect(within(jobs).getByText("Merge: net-dhcp")).toBeInTheDocument();
    expect(within(jobs).getByText("Deploy: secretary-server")).toBeInTheDocument();
    expect(within(jobs).getByText("Queued")).toBeInTheDocument();
    // Moving work first, stale last.
    const heads = screen.getAllByRole("heading", { level: 4 }).map((h) => h.textContent);
    expect(heads).toEqual(["Merging (1)", "Open (1)", "Stale (1)"]);
    const merging = screen.getByRole("list", { name: "Merging" });
    expect(within(merging).getByText("DHCP on the stick")).toBeInTheDocument();
    expect(within(merging).getByText(/LucinaAndroidOS · net-dhcp · \+2/)).toBeInTheDocument();
    expect(within(merging).getByText(/local_ff6cd29a…/)).toHaveTextContent("5 processes · 33 MB · bash×3 docker×1");
    const open = screen.getByRole("list", { name: "Open" });
    expect(within(open).getByText(/\+1 · uncommitted changes/)).toBeInTheDocument();
    expect(within(open).getByText("no session")).toBeInTheDocument();
    expect(within(screen.getByRole("list", { name: "Stale" })).getByText(/idle 9 d/)).toBeInTheDocument();
    expect(within(screen.getByRole("list", { name: "Recent jobs" })).getByText(/GATE_FAILED/)).toBeInTheDocument();
    expect(within(screen.getByRole("list", { name: "Devices in use" })).getByText("iphone")).toBeInTheDocument();
    expect(within(screen.getByRole("list", { name: "Devices in use" })).queryByText("stick")).not.toBeInTheDocument();
    expect(screen.getByText("web f74841ed")).toBeInTheDocument();
    expect(screen.getByText("1 worktrees are not registered")).toBeInTheDocument();
    expect(calls.filter((c) => c.method !== "GET")).toEqual([]);
  });

  it("says when the server has no pipeline route", async () => {
    serveAgents({ absent: ["/search", "/schedules", "/pipeline"] });
    renderAgents("/agents/pipeline");
    expect(await screen.findByText("Not available on this server yet.")).toBeInTheDocument();
  });

  it("shows why the board can't be read", async () => {
    serveAgents({ absent: ["/search", "/schedules"], extra: (c) => (c.url === "/v1/agents/pipeline/board" ? { status: 503, body: { error: { code: "agentd_unavailable", message: "lucina-agentd is not running" } } } : undefined) });
    renderAgents("/agents/pipeline");
    expect(await screen.findByText("Couldn't read the pipeline: lucina-agentd is not running")).toHaveAttribute("role", "alert");
  });
});

describe("agent chat", () => {
  const timeline = [
    row(1, { type: "user_message", text: "Fix the failing build" }),
    row(2, { type: "reasoning", text: "Looking at the logs" }),
    row(3, { type: "assistant_message", text: "I'll check **the logs** " }),
    row(4, { type: "assistant_message", text: "first." }),
    row(5, { type: "tool_call", callId: "c1", name: "Bash", status: "running", detail: { type: "shell", command: "npm test" } }),
    row(6, { type: "tool_call", callId: "c1", name: "Bash", status: "completed", detail: { type: "shell", command: "npm test", output: "3 passed", exitCode: 0 } }),
    row(7, { type: "error", message: "Something odd" }),
  ];

  const sendOk = (c: { url: string; method: string }) =>
    c.method === "POST" && c.url === "/v1/agents/a1/send" ? { body: { agentId: "a1", disposition: "started" } } : c.method === "POST" && c.url === "/v1/agents/a1/stop" ? { body: { agentId: "a1", stopped: true } } : undefined;

  function setup(over: Partial<ReturnType<typeof snapshot>> = {}, extra: Fake["extra"] = sendOk, absent: string[] = ["/search", "/schedules", "/terminals", "/a1/prompts"]) {
    return serveAgents({
      extra,
      items: [item("a1", { title: "Fix the build", status: over.status ?? "idle" })],
      snapshots: { a1: snapshot("a1", { title: "Fix the build", ...over }) },
      timelines: { a1: { rows: timeline } },
      absent,
    });
  }

  it("renders messages, collapsed tool calls and errors from the timeline", async () => {
    setup();
    renderAgents("/agents/a1");
    expect(await screen.findByText("Fix the failing build")).toBeInTheDocument();
    // /prompts answers 404 here: there is no Prompts button.
    await waitFor(() => expect(screen.queryByRole("button", { name: "Prompts" })).not.toBeInTheDocument());
    // Streamed chunks are one message, with markdown.
    const strong = screen.getByText("the logs");
    expect(strong.tagName).toBe("STRONG");
    expect(strong.closest(".ag-msg--assistant")).toHaveTextContent("I'll check the logs first.");
    // The tool call is one collapsed row: title and command, the output only once opened.
    const tool = document.querySelector("details.ag-tool") as HTMLDetailsElement;
    expect(tool).not.toBeNull();
    expect(document.querySelectorAll("details.ag-tool")).toHaveLength(1);
    expect(tool.open).toBe(false);
    expect(within(tool).getByText("Shell")).toBeInTheDocument();
    expect(tool.querySelector(".ag-tool__sum")).toHaveTextContent("npm test");
    expect(within(tool).getByText("3 passed")).toBeInTheDocument();
    expect(screen.getByText("Something odd")).toHaveAttribute("role", "alert");
    expect(screen.getByText("Looking at the logs").closest("details")).not.toHaveAttribute("open");
    expect(screen.getByText("Something odd")).toBeInTheDocument();
  });

  it("appends timeline rows from the stream, and refetches after a gap", async () => {
    const calls = setup();
    renderAgents("/agents/a1");
    await screen.findByText("Fix the failing build");
    const { send } = stream();
    send({ type: "agent_stream", agentId: "a1", event: { type: "timeline", item: { type: "assistant_message", text: " More." } }, seq: 8, epoch: "e1", timestamp: "2026-10-28T12:01:00Z" }, "1");
    await waitFor(() => expect([...document.querySelectorAll(".ag-msg--assistant")].at(-1)).toHaveTextContent("More."));
    // A duplicate is ignored.
    send({ type: "agent_stream", agentId: "a1", event: { type: "timeline", item: { type: "error", message: "dup" } }, seq: 8, epoch: "e1", timestamp: "x" }, "2");
    expect(screen.queryByText("dup")).not.toBeInTheDocument();
    // A gap (seq 12 after 8) fetches what is missing.
    const n = calls.filter((c) => c.url.includes("/a1/timeline")).length;
    send({ type: "agent_stream", agentId: "a1", event: { type: "timeline", item: { type: "error", message: "gap" } }, seq: 12, epoch: "e1", timestamp: "x" }, "3");
    await waitFor(() => expect(calls.filter((c) => c.url.includes("/a1/timeline")).length).toBeGreaterThan(n));
    expect(calls.filter((c) => c.url.includes("/a1/timeline")).at(-1)!.url).toContain("afterSeq=8");
  });

  it("sends a message while idle", async () => {
    const calls = setup();
    renderAgents("/agents/a1");
    await screen.findByText("Fix the failing build");
    const box = screen.getByRole("textbox", { name: "Message to the agent" });
    fireEvent.input(box, { target: { value: "  now the docs " } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(calls.some((c) => c.url.endsWith("/send"))).toBe(true));
    const post = calls.find((c) => c.url.endsWith("/send"))!;
    expect(post.body).toEqual({ prompt: "now the docs" });
    await waitFor(() => expect(box).toHaveValue(""));
    expect(post.headers["x-secretary-csrf"]).toBe("1");
  });

  it("steers a running agent and can stop it", async () => {
    const calls = setup({ status: "running" });
    renderAgents("/agents/a1");
    await screen.findByText("Fix the failing build");
    const box = screen.getByRole("textbox", { name: "Steer the running agent" });
    fireEvent.input(box, { target: { value: "use pnpm" } });
    fireEvent.click(screen.getByRole("button", { name: "Steer" }));
    await waitFor(() => expect(calls.some((c) => c.url.endsWith("/send"))).toBe(true));
    expect(calls.find((c) => c.url.endsWith("/send"))!.body).toEqual({ prompt: "use pnpm", activeTurnBehavior: "steer" });
    fireEvent.click(screen.getAllByRole("button", { name: "Stop" })[0]!);
    await waitFor(() => expect(calls.some((c) => c.method === "POST" && c.url === "/v1/agents/a1/stop")).toBe(true));
  });

  it("shows the send error and keeps the text", async () => {
    setup({}, (c) => (c.url === "/v1/agents/a1/send" ? { status: 409, body: { error: { code: "AGENT_BUSY", message: "The agent is busy." } } } : undefined));
    renderAgents("/agents/a1");
    await screen.findByText("Fix the failing build");
    const box = screen.getByRole("textbox", { name: "Message to the agent" });
    fireEvent.input(box, { target: { value: "hello" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(await screen.findByText("The agent is busy.")).toBeInTheDocument();
    expect(box).toHaveValue("hello");
  });

  it("renames, archives and unarchives", async () => {
    const calls = setup({}, (c) =>
      c.method === "PATCH" && c.url === "/v1/agents/a1" ? { body: snapshot("a1", { title: "Build fixed" }) } : c.method === "POST" && c.url === "/v1/agents/a1/archive" ? { body: { agentId: "a1", archivedAt: "2026-10-28T12:00:00Z" } } : undefined,
    );
    renderAgents("/agents/a1");
    await screen.findByText("Fix the failing build");
    calls.length = 0;
    fireEvent.click(screen.getByRole("button", { name: "Rename" }));
    const input = screen.getAllByRole("textbox").find((e) => (e as HTMLInputElement).maxLength === 200)!;
    fireEvent.input(input, { target: { value: "Build fixed" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(calls.some((c) => c.method === "PATCH" && c.url === "/v1/agents/a1")).toBe(true));
    expect(calls.find((c) => c.method === "PATCH")!.body).toEqual({ title: "Build fixed" });
    fireEvent.click(screen.getByRole("button", { name: "Archive" }));
    await waitFor(() => expect(calls.some((c) => c.method === "POST" && c.url === "/v1/agents/a1/archive")).toBe(true));
  });

  it("says when the agent does not exist", async () => {
    serveAgents({ absent: ["/search", "/schedules"] });
    renderAgents("/agents/nope");
    expect(await screen.findByText("No agent with that id.")).toBeInTheDocument();
  });
});

describe("permissions", () => {
  const pending = permission({ approvalId: "apv_9" });

  function setup(extraRoutes?: Fake["extra"]) {
    return serveAgents({
      items: [item("a1", { title: "Fix the build", bucket: "needs_input", status: "running" })],
      snapshots: {
        a1: snapshot("a1", {
          status: "running",
          pendingPermissions: [{ id: "req_1", provider: "claude", name: "Bash", kind: "tool", detail: { type: "shell", command: "npm test" } }],
        }),
      },
      permissions: [pending],
      absent: ["/search", "/schedules", "/terminals", "/a1/prompts"],
      extra: extraRoutes,
    });
  }

  it("shows a pending request inline with the exact command and a link to the approvals", async () => {
    setup();
    renderAgents("/agents/a1");
    const card = await screen.findByRole("article", { name: "Run: npm test" });
    expect(within(card).getByText("npm test")).toBeInTheDocument();
    expect(within(card).getByRole("link", { name: "Open in approvals" })).toHaveAttribute("href", "/approvals/apv_9");
    expect(within(card).getByRole("button", { name: "Allow" })).toBeInTheDocument();
  });

  it("allows an agent request through the agent's route", async () => {
    const calls = setup((c) => (c.url === "/v1/agents/a1/permissions/req_1" ? { body: { agentId: "a1", requestId: "req_1", behavior: "allow" } } : undefined));
    renderAgents("/agents/a1");
    const card = await screen.findByRole("article", { name: "Run: npm test" });
    fireEvent.click(within(card).getByRole("button", { name: "Allow" }));
    await waitFor(() => expect(within(card).getByText("Allowed")).toBeInTheDocument());
    const post = calls.find((c) => c.url === "/v1/agents/a1/permissions/req_1")!;
    expect(post.method).toBe("POST");
    expect(post.body).toEqual({ behavior: "allow" });
    expect(post.headers["x-secretary-csrf"]).toBe("1");
  });

  it("denies one, and an external request goes to /permissions/{id}", async () => {
    const calls = serveAgents({
      items: [],
      permissions: [permission({ id: "ext_1", kind: "external", agentId: undefined, source: "infisical-brokerd", title: "Reveal secret DB_URL", details: { secret: "DB_URL" } })],
      absent: ["/search", "/schedules"],
      extra: (c) => (c.url === "/v1/agents/permissions/ext_1" && c.method === "POST" ? { body: { id: "ext_1", status: "denied" } } : undefined),
    });
    renderAgents("/agents/permissions/ext_1");
    const card = await screen.findByRole("article", { name: "Reveal secret DB_URL" });
    expect(card.closest(".ag-permwrap")).toHaveAttribute("data-focus", "yes");
    fireEvent.click(within(card).getByRole("button", { name: "Deny" }));
    await waitFor(() => expect(within(card).getByText("Denied")).toBeInTheDocument());
    expect(calls.find((c) => c.url === "/v1/agents/permissions/ext_1" && c.method === "POST")!.body).toEqual({ behavior: "deny" });
  });

  it.each([
    [409, "Already answered elsewhere."],
    [410, "This request expired."],
    [404, "This request is gone."],
  ])("closes the card gracefully on %i", async (status, text) => {
    setup((c) => (c.url === "/v1/agents/a1/permissions/req_1" ? { status, body: { error: { code: "X", message: "x" } } } : undefined));
    renderAgents("/agents/a1");
    const card = await screen.findByRole("article", { name: "Run: npm test" });
    fireEvent.click(within(card).getByRole("button", { name: "Allow" }));
    expect(await within(card).findByText(text)).toBeInTheDocument();
    expect(within(card).queryByRole("button", { name: "Allow" })).not.toBeInTheDocument();
  });

  it("keeps the buttons and explains a 403", async () => {
    setup((c) => (c.url === "/v1/agents/a1/permissions/req_1" ? { status: 403, body: { error: { code: "PERMISSION_FORBIDDEN", message: "no" } } } : undefined));
    renderAgents("/agents/a1");
    const card = await screen.findByRole("article", { name: "Run: npm test" });
    fireEvent.click(within(card).getByRole("button", { name: "Deny" }));
    expect(await within(card).findByText("This device may not answer it.")).toBeInTheDocument();
    expect(within(card).getByRole("button", { name: "Allow" })).toBeEnabled();
  });

  it("lists every pending request on the permissions page, with the agent's name", async () => {
    setup();
    renderAgents("/agents/permissions");
    const card = await screen.findByRole("article", { name: "Run: npm test" });
    expect(within(card).getByText("Fix the build")).toBeInTheDocument();
    expect(agentsState.permissions["a1/req_1"]?.approvalId).toBe("apv_9");
  });
});

describe("new run", () => {
  it("starts an agent in an existing workspace", async () => {
    const calls = serveAgents({
      absent: ["/search", "/schedules"],
      extra: (c) => (c.url === "/v1/agents" && c.method === "POST" ? { body: snapshot("n1", { title: "New one" }) } : undefined),
    });
    renderAgents("/agents/new");
    expect(await screen.findByRole("heading", { name: "New run" })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Provider" })).toHaveValue("claude"));
    fireEvent.change(screen.getByRole("combobox", { name: "Mode" }), { target: { value: "acceptEdits" } });
    fireEvent.input(screen.getByRole("textbox", { name: "Prompt" }), { target: { value: "Add tests" } });
    fireEvent.click(screen.getByRole("button", { name: "Start" }));
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/agents" && c.method === "POST")).toBe(true));
    expect(calls.find((c) => c.url === "/v1/agents" && c.method === "POST")!.body).toEqual({
      provider: "claude",
      cwd: "/home/nez/Projects/demo",
      workspaceId: "wks_1",
      prompt: "Add tests",
      modeId: "acceptEdits",
    });
  });

  it("makes a worktree first when a project is picked", async () => {
    const calls = serveAgents({
      absent: ["/search", "/schedules"],
      extra: (c) => {
        if (c.url === "/v1/agents/workspaces" && c.method === "POST") return { body: { workspace: workspace({ workspaceId: "wks_new", cwd: "/home/nez/wt/x", name: "x", isolation: "worktree" }) } };
        if (c.url === "/v1/agents" && c.method === "POST") return { body: snapshot("n2") };
        return undefined;
      },
    });
    renderAgents("/agents/new");
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Workspace" })).toBeInTheDocument());
    await screen.findByRole("option", { name: "New worktree in demo" });
    fireEvent.change(screen.getByRole("combobox", { name: "Workspace" }), { target: { value: "new:prj_1" } });
    fireEvent.input(screen.getByRole("textbox", { name: "Prompt" }), { target: { value: "Refactor" } });
    fireEvent.click(screen.getByRole("button", { name: "Start" }));
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/agents" && c.method === "POST")).toBe(true));
    expect(calls.find((c) => c.url === "/v1/agents/workspaces" && c.method === "POST")!.body).toEqual({ isolation: "worktree", projectId: "prj_1", prompt: "Refactor", provider: "claude" });
    expect(calls.find((c) => c.url === "/v1/agents" && c.method === "POST")!.body).toMatchObject({ cwd: "/home/nez/wt/x", workspaceId: "wks_new" });
  });

  it("shows the daemon's refusal", async () => {
    serveAgents({
      absent: ["/search", "/schedules"],
      extra: (c) => (c.url === "/v1/agents" && c.method === "POST" ? { status: 400, body: { error: { code: "WORKSPACE_GUARD", message: "That is a main checkout." } } } : undefined),
    });
    renderAgents("/agents/new");
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Provider" })).toHaveValue("claude"));
    fireEvent.click(screen.getByRole("button", { name: "Start" }));
    expect(await screen.findByText("That is a main checkout.")).toBeInTheDocument();
  });
});

describe("changes and files", () => {
  const diff = {
    cwd: "/home/nez/Projects/demo",
    diffTooLarge: false,
    error: null,
    files: [
      {
        path: "src/main.rs",
        isNew: false,
        isDeleted: false,
        additions: 2,
        deletions: 1,
        status: "ok",
        hunks: [{ oldStart: 1, oldCount: 2, newStart: 1, newCount: 3, lines: [{ type: "context", content: "fn main() {" }, { type: "remove", content: '    println!("a");' }, { type: "add", content: '    println!("b");' }, { type: "add", content: "}" }] }],
      },
      { path: "logo.png", isNew: true, isDeleted: false, additions: 0, deletions: 0, status: "binary", hunks: [] },
    ],
  };

  function setup() {
    return serveAgents({
      items: [item("a1")],
      snapshots: { a1: snapshot("a1") },
      absent: ["/search", "/schedules", "/terminals", "/a1/prompts"],
      extra: (c) => {
        if (c.url.startsWith("/v1/agents/workspaces/wks_1/diff")) return { body: diff };
        if (c.url.startsWith("/v1/agents/workspaces/wks_1/files")) return { body: { path: "", entries: [{ name: "src", path: "src", kind: "directory", size: 0, modifiedAt: "x" }, { name: "README.md", path: "README.md", kind: "file", size: 12, modifiedAt: "x" }] } };
        if (c.url.startsWith("/v1/agents/workspaces/wks_1/file?")) return { body: { path: "README.md", kind: "text", encoding: "utf-8", content: "# Demo\nhello", size: 12, modifiedAt: "x" } };
        if (c.url === "/v1/agents/workspaces/wks_1/download-token") return { body: { token: "tok1", url: "/files/download?token=tok1", expiresInSec: 60 } };
        return undefined;
      },
    });
  }

  it("lists changed files and shows a readable diff, in the three modes", async () => {
    const calls = setup();
    renderAgents("/agents/a1/changes");
    const grid = await screen.findByRole("list", { name: "Changed files" });
    expect(within(grid).getByText("main.rs")).toBeInTheDocument();
    expect(within(grid).getByText("logo.png")).toBeInTheDocument();
    const lines = [...document.querySelectorAll(".ag-dl")].map((l) => [l.getAttribute("data-t"), l.querySelector(".ag-dl__c")!.textContent]);
    expect(lines).toEqual([
      ["hunk", "@@ -1,2 +1,3 @@"],
      ["context", "fn main() {"],
      ["remove", '    println!("a");'],
      ["add", '    println!("b");'],
      ["add", "}"],
    ]);
    fireEvent.click(within(grid).getByText("logo.png"));
    expect(await screen.findByText("Binary file.")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Uncommitted" }));
    await waitFor(() => expect(calls.some((c) => c.url.includes("/diff?mode=uncommitted"))).toBe(true));
    fireEvent.click(screen.getByRole("button", { name: "Since base" }));
    await waitFor(() => expect(calls.some((c) => c.url.includes("/diff?mode=base"))).toBe(true));
    expect(calls.some((c) => c.url.includes("/diff?mode=all"))).toBe(true);
  });

  it("browses files, reads one and offers a download", async () => {
    const calls = setup();
    renderAgents("/agents/a1/files");
    fireEvent.click(await screen.findByText("README.md"));
    expect(await screen.findByText(/# Demo/)).toBeInTheDocument();
    expect(calls.some((c) => c.url.startsWith("/v1/agents/workspaces/wks_1/file?path=README.md"))).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Download" }));
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/agents/workspaces/wks_1/download-token" && c.method === "POST")).toBe(true));
    expect(calls.find((c) => c.url === "/v1/agents/workspaces/wks_1/download-token")!.body).toEqual({ path: "README.md" });
  });
});

describe("terminals, scripts and the other phase 5 and 6 routes", () => {
  const base = { items: [item("a1")], snapshots: { a1: snapshot("a1") } };

  it("has no Terminal or Scripts tab while the routes are not deployed", async () => {
    serveAgents({ ...base, absent: ["/search", "/schedules", "/terminals", "/workspaces/wks_1/scripts"] });
    renderAgents("/agents/a1");
    await screen.findByRole("navigation", { name: "Sections", hidden: false }).catch(() => undefined);
    await screen.findByRole("link", { name: "Changes" });
    await waitFor(() => expect(agentsState.workspaces["wks_1"]).toBeDefined());
    await new Promise((r) => setTimeout(r, 30));
    expect(screen.queryByRole("link", { name: "Terminal" })).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Scripts" })).not.toBeInTheDocument();
  });

  it("shows them when GET /terminals answers, streams the output over SSE and sends input", async () => {
    const calls = serveAgents({
      ...base,
      absent: ["/search", "/schedules"],
      extra: (c) => {
        if (c.url === "/v1/agents/terminals" && c.method === "GET") return { body: { terminals: [{ id: "term_1", name: "dev", title: "dev", cwd: "/x", workspaceId: "wks_1", rows: 24, cols: 80, status: "running" }] } };
        if (c.url === "/v1/agents/terminals/term_1/input") return { body: { terminalId: "term_1", keysSent: 2 } };
        if (c.url === "/v1/agents/workspaces/wks_1/scripts") return { body: { workspaceId: "wks_1", scripts: [{ scriptName: "web", type: "service", lifecycle: "running", port: 4123, health: "healthy", command: "npm run dev", servicePath: "/v1/agents/workspaces/wks_1/services/web/" }] } };
        if (c.url === "/v1/agents/workspaces/wks_1/services") return { body: { workspaceId: "wks_1", services: [{ name: "port-4173", kind: "detected", port: 4173, path: "/v1/agents/workspaces/wks_1/services/port-4173/" }] } };
        if (c.url === "/v1/agents/workspaces/wks_1/scripts/web/stop") return { body: { scriptName: "web", type: "service", lifecycle: "stopped" } };
        return undefined;
      },
    });
    renderAgents("/agents/a1/terminal");
    await screen.findByRole("link", { name: "Terminal" });
    await screen.findByRole("textbox", { name: "Type a command" });
    const term = ES.instances.find((e) => e.url.startsWith("/v1/agents/terminals/term_1/stream"))!;
    term.open();
    term.emit("snapshot", { rows: 24, cols: 80, data: "\u001b[32mready\u001b[0m\r\n$ ", offset: 10 });
    const out = screen.getByLabelText("Terminal output");
    expect(out).toHaveTextContent("ready");
    term.emit("output", { b64: btoa("echo hi\r\nhi\r\n") }, "24");
    await waitFor(() => expect(out).toHaveTextContent(/echo hi\s+hi/));
    expect(screen.getByText("Connected")).toBeInTheDocument();
    fireEvent.input(screen.getByRole("textbox", { name: "Type a command" }), { target: { value: "ls -la" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/agents/terminals/term_1/input")).toBe(true));
    expect(calls.find((c) => c.url === "/v1/agents/terminals/term_1/input")!.body).toEqual({ keys: ["ls -la", "Enter"] });
    fireEvent.click(screen.getByRole("button", { name: "Ctrl-C" }));
    await waitFor(() => expect(calls.filter((c) => c.url === "/v1/agents/terminals/term_1/input")).toHaveLength(2));
    expect(calls.filter((c) => c.url === "/v1/agents/terminals/term_1/input")[1]!.body).toEqual({ keys: "C-c" });
    term.emit("exit", { exitCode: 0 });
    expect(await screen.findByText("The terminal exited (code 0).")).toBeInTheDocument();

    // Scripts: status, a link to the service proxy, stop.
    fireEvent.click(screen.getByRole("link", { name: "Scripts" }));
    expect(await screen.findByText("npm run dev")).toBeInTheDocument();
    const open = screen.getAllByRole("link", { name: "Open" }).map((a) => a.getAttribute("href"));
    expect(open).toContain("/v1/agents/workspaces/wks_1/services/web/");
    expect(open).toContain("/v1/agents/workspaces/wks_1/services/port-4173/");
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/agents/workspaces/wks_1/scripts/web/stop" && c.method === "POST")).toBe(true));
  });

  it("lists schedules with pause, resume and run now", async () => {
    const sch = (status: string) => ({ id: "s1", name: "Morning check", prompt: "p", cadence: { type: "cron", expression: "0 7 * * 1-5" }, target: { type: "agent", agentId: "a1" }, status, nextRunAt: "2026-10-29T05:00:00Z", lastRunAt: null });
    const calls = serveAgents({
      ...base,
      absent: ["/search"],
      extra: (c) => {
        if (c.url === "/v1/agents/schedules") return { body: { schedules: [sch("active")] } };
        if (c.url === "/v1/agents/schedules/s1/pause") return { body: sch("paused") };
        if (c.url === "/v1/agents/schedules/s1/resume") return { body: sch("active") };
        if (c.url === "/v1/agents/schedules/s1/run-once") return { body: sch("active") };
        return undefined;
      },
    });
    renderAgents("/agents/schedules");
    expect(await screen.findByText("Morning check")).toBeInTheDocument();
    expect(screen.getByText("0 7 * * 1-5")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Pause" }));
    expect(await screen.findByRole("button", { name: "Resume" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Run now" }));
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/agents/schedules/s1/run-once" && c.method === "POST")).toBe(true));
  });

  it("jumps to a prompt from the prompt index (phase 6)", async () => {
    serveAgents({
      ...base,
      timelines: { a1: { rows: [row(1, { type: "user_message", text: "first ask" }), row(2, { type: "assistant_message", text: "done" })] } },
      absent: ["/search", "/schedules", "/terminals"],
      extra: (c) => (c.url === "/v1/agents/a1/prompts" ? { body: { epoch: "e1", prompts: [{ seq: 1, timestamp: "x", preview: "first ask" }] } } : undefined),
    });
    renderAgents("/agents/a1");
    await screen.findByText("done");
    const open = await screen.findByRole("button", { name: "Prompts" });
    // It lives in the header beside Archive, not over the conversation.
    expect(open.closest(".ag-head__actions")).not.toBeNull();
    expect(open.closest(".ag-scroll")).toBeNull();
    fireEvent.click(open);
    fireEvent.click(await screen.findByRole("button", { name: "first ask" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "first ask" })).not.toBeInTheDocument());
  });

  it("detaches a subagent from its parent", async () => {
    const calls = serveAgents({
      items: [item("a1", { labels: { "lucina.parent-agent-id": "p0" } })],
      snapshots: { a1: snapshot("a1", { labels: { "lucina.parent-agent-id": "p0" } }) },
      absent: ["/search", "/schedules", "/terminals", "/a1/prompts"],
      extra: (c) => (c.url === "/v1/agents/a1/detach" ? { body: snapshot("a1") } : undefined),
    });
    renderAgents("/agents/a1");
    fireEvent.click(await screen.findByRole("button", { name: "Detach from parent" }));
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/agents/a1/detach" && c.method === "POST")).toBe(true));
  });
});

describe("usage", () => {
  it("shows the server's report", async () => {
    serveAgents({
      items: [],
      absent: ["/search", "/schedules"],
      extra: (c) => (c.url === "/v1/agents/usage" ? { body: { providers: [{ provider: "claude", agents: 3, running: 1, inputTokens: 1_200_000, cachedInputTokens: 800_000, outputTokens: 45_000, totalCostUsd: 12.5 }], schedules: { active: 2, paused: 1, completed: 0 } } } : undefined),
    });
    renderAgents("/agents/usage");
    const table = await screen.findByRole("table");
    expect(within(table).getByText("claude")).toBeInTheDocument();
    expect(within(table).getByText("$12.50")).toBeInTheDocument();
    expect(within(table).getByText("1.2M")).toBeInTheDocument();
    expect(await screen.findByText("Schedules: 2 active, 1 paused, 0 completed.")).toBeInTheDocument();
  });

  it("sums the agents' own usage when the route is missing", async () => {
    serveAgents({
      items: [],
      absent: ["/search", "/schedules", "/usage"],
      extra: (c) =>
        c.url === "/v1/agents"
          ? { body: { agents: [snapshot("a1", { lastUsage: { inputTokens: 1000, outputTokens: 500, totalCostUsd: 0.25 } }), snapshot("a2", { lastUsage: { inputTokens: 3000, outputTokens: 100, totalCostUsd: 0.5 } })] } }
          : undefined,
    });
    renderAgents("/agents/usage");
    const table = await screen.findByRole("table");
    expect(within(table).getByText("$0.75")).toBeInTheDocument();
    expect(within(table).getByText("4K")).toBeInTheDocument();
  });
});
