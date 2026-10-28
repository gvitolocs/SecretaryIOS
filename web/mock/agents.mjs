// Lucina Agents for the mock server (/v1/agents, contract: docs/design/agents-api.md on server-live; phase 5/6 routes:
// the sessions' notes agents-p5-routes.md / agents-p6-ui.md). In memory, one shared SSE stream, everything the
// web plugin calls:
//   agents:      5 agents (needs permission, running, finished, failed, closed) with timelines
//   POST send:   appends your message and, a moment later, a streamed reply (over the stream)
//   permission:  one pending Bash request (with an approvalId); answering it resumes the agent
//   workspaces:  two workspaces, a 3-file diff in the three modes, a small file tree
//   phase 5:     terminals (SSE output) and scripts/services
//   phase 6:     search, prompts, schedules (pause/resume/run-once), usage
// MOCK_AGENTS=absent answers 404 for the phase 5 and 6 routes.

const now = Date.now();
const iso = (minAgo) => new Date(now - minAgo * 60_000).toISOString();
const absent = process.env.MOCK_AGENTS === "absent";

const WORKSPACES = {
  wks_1: { workspaceId: "wks_1", projectId: "prj_1", cwd: "/home/nez/Projects/.worktrees/SecretaryIOS/fix-build", kind: "worktree", name: "fix-build", displayName: "fix-build", projectDisplayName: "SecretaryIOS", isolation: "worktree", branch: "fix-build", background: false, createdAt: iso(300), updatedAt: iso(2) },
  wks_2: { workspaceId: "wks_2", projectId: "prj_2", cwd: "/home/nez/Projects/Lucina/lucina-rs", kind: "local", name: "main", displayName: "main", projectDisplayName: "lucina-rs", isolation: "local", branch: "main", background: false, createdAt: iso(900), updatedAt: iso(30) },
};

const snap = (id, o) => ({
  id,
  provider: "claude",
  cwd: WORKSPACES[o.workspaceId ?? "wks_1"].cwd,
  workspaceId: "wks_1",
  model: "opus-5.5",
  createdAt: iso(120),
  updatedAt: iso(3),
  lastUserMessageAt: iso(4),
  status: "idle",
  pendingPermissions: [],
  availableModes: [],
  labels: {},
  title: id,
  ...o,
});

let AGENTS = {
  a1c2d3e4: snap("a1c2d3e4", { title: "Fix the failing build", status: "running", updatedAt: iso(1), pendingPermissions: [{ id: "req_1", provider: "claude", name: "Bash", kind: "tool", detail: { type: "shell", command: "npm test -- --run" } }], requiresAttention: true, attentionReason: "permission", lastUsage: { inputTokens: 184_200, cachedInputTokens: 150_100, outputTokens: 9_400, totalCostUsd: 1.84 } }),
  b2d3e4f5: snap("b2d3e4f5", { title: "Write the docs site", provider: "codex", model: "gpt-6.1-sol", workspaceId: "wks_2", status: "running", updatedAt: iso(0.5), lastUsage: { inputTokens: 92_000, outputTokens: 12_100, totalCostUsd: 0.62 } }),
  c3e4f5a6: snap("c3e4f5a6", { title: "Refactor the parser", provider: "opencode", model: "deepseek-v4.1-flash", workspaceId: "wks_2", status: "idle", updatedAt: iso(18), requiresAttention: true, attentionReason: "finished", gate: { verdict: "verified", details: [], proofCommand: "cargo test -p parser", round: 0 }, lastUsage: { inputTokens: 40_300, outputTokens: 5_100, totalCostUsd: 0 } }),
  d4f5a6b7: snap("d4f5a6b7", { title: "Bump dependencies", status: "error", updatedAt: iso(55), lastError: "Provider run failed: usage limit reached (resets 14:00).", lastUsage: { inputTokens: 12_000, outputTokens: 800, totalCostUsd: 0.2 } }),
  e5a6b7c8: snap("e5a6b7c8", { title: "Old chore", provider: "cursor", model: "grok-4.7-high", status: "closed", updatedAt: iso(60 * 26), lastUsage: { inputTokens: 5000, outputTokens: 700, totalCostUsd: 0.1 } }),
};

const bucketOf = (s) => ((s.pendingPermissions?.length ?? 0) > 0 || s.attentionReason === "permission" ? "needs_input" : s.status === "error" ? "failed" : s.status === "running" ? "running" : s.requiresAttention ? "attention" : "done");
const itemOf = (s) => ({ id: s.id, shortId: s.id.slice(0, 7), title: s.title, provider: s.provider, model: s.model, status: s.status, cwd: s.cwd, createdAt: s.createdAt, updatedAt: s.updatedAt, lastUserMessageAt: s.lastUserMessageAt, archivedAt: s.archivedAt ?? null, requiresAttention: s.requiresAttention, attentionReason: s.attentionReason, labels: s.labels, bucket: bucketOf(s) });

const tool = (callId, name, detail, status = "completed") => ({ type: "tool_call", callId, name, status, detail, error: null });
const TIMELINES = {
  a1c2d3e4: [
    { type: "user_message", text: "The CI build is red on main. Find out why and fix it, then run the tests." },
    { type: "reasoning", text: "Start with the failing job's log, then reproduce locally." },
    { type: "assistant_message", text: "I'll start with the **CI log**, then reproduce the failure locally.\n\n1. Read the failing job\n2. Run the tests\n3. Fix and re-run" },
    tool("c1", "Bash", { type: "shell", command: "gh run view --log-failed | tail -40", output: "web/src/views/Keep.test.tsx > opens a note\nTypeError: Cannot read properties of undefined (reading 'length')\n  at loadTimeline (state/agents.ts:130)", exitCode: 0 }),
    tool("c2", "Read", { type: "read", filePath: "web/src/state/agents.ts", content: "export async function loadTimeline(id: string) { ... }" }),
    { type: "assistant_message", text: "The failure is a **stale timeline entry** that has no `rows` array. I'll make `loadTimeline` tolerate it and add a regression test." },
    tool("c3", "Edit", { type: "edit", filePath: "web/src/state/agents.ts", unifiedDiff: "@@ -125,3 +125,3 @@\n-  if (!cur) setState(...)\n+  if (!cur?.rows) setState(...)" }),
    tool("c4", "Bash", { type: "shell", command: "npm test -- --run" }, "running"),
  ],
  b2d3e4f5: [
    { type: "user_message", text: "Draft the docs site structure from the design notes." },
    { type: "assistant_message", text: "Reading the design notes, then I'll lay out the pages." },
    { type: "todo", items: [{ text: "Read docs/design", completed: true }, { text: "Outline pages", completed: false, status: "in_progress" }, { text: "Write the intro", completed: false }] },
  ],
  c3e4f5a6: [
    { type: "user_message", text: "Refactor the parser so the lexer is a separate module." },
    tool("c1", "Edit", { type: "edit", filePath: "crates/parser/src/lib.rs", unifiedDiff: "@@\n-mod lex;\n+pub mod lexer;" }),
    { type: "assistant_message", text: "Done. The lexer now lives in `lexer.rs`; all 41 tests pass." },
  ],
  d4f5a6b7: [
    { type: "user_message", text: "Bump all dependencies to the latest minor versions." },
    { type: "error", message: "Provider run failed: usage limit reached (resets 14:00)." },
  ],
  e5a6b7c8: [{ type: "user_message", text: "Tidy the scripts folder." }, { type: "assistant_message", text: "Tidied." }],
};
const ROWS = Object.fromEntries(Object.entries(TIMELINES).map(([id, items]) => [id, items.map((item, i) => ({ seq: i + 1, timestamp: iso(40 - i), item, turnId: "t1" }))]));

let PERMS = [
  { id: "req_1", kind: "agent", agentId: "a1c2d3e4", name: "Bash", title: "Run: npm test -- --run", description: "Claude wants to run a shell command in fix-build.", details: { command: "npm test -- --run", timeout: 120000 }, status: "pending", createdAt: iso(1), expiresAt: new Date(now + 9 * 60_000).toISOString(), approvalId: "apv_demo1" },
  { id: "ext_1", kind: "external", source: "infisical-brokerd", title: "Reveal secret DB_URL to the deploy script", details: { secret: "DB_URL", requester: "deploy.sh" }, status: "pending", createdAt: iso(3), expiresAt: new Date(now + 4 * 60_000).toISOString() },
  { id: "req_0", kind: "agent", agentId: "c3e4f5a6", name: "Edit", title: "Edit crates/parser/src/lib.rs", status: "allowed", createdAt: iso(25), expiresAt: iso(15), resolution: { behavior: "allow", by: "owner_device", at: iso(24) } },
];

const DIFF = {
  cwd: WORKSPACES.wks_1.cwd,
  diffTooLarge: false,
  error: null,
  files: [
    { path: "web/src/state/agents.ts", isNew: false, isDeleted: false, additions: 5, deletions: 2, status: "ok", hunks: [{ oldStart: 123, oldCount: 8, newStart: 123, newCount: 11, lines: [
      { type: "context", content: "export async function loadTimeline(id: string): Promise<void> {" },
      { type: "context", content: "  const cur = state.timelines[id];" },
      { type: "context", content: '  if (cur?.status === "loading") return;' },
      { type: "remove", content: '  if (!cur) setState("timelines", id, { epoch: "", rows: [], status: "loading" });' },
      { type: "add", content: "  const gen = generation;" },
      { type: "add", content: '  if (!cur?.rows) setState("timelines", id, { epoch: "", rows: [], status: "loading" });' },
      { type: "context", content: '  else setState("timelines", id, "status", "loading");' },
      { type: "remove", content: "      const page = await agentsApi.timeline(id, after, 500);" },
      { type: "add", content: "      const page = await agentsApi.timeline(id, after, 500);" },
      { type: "add", content: "      if (gen !== generation || !state.timelines[id]) return;" },
      { type: "add", content: "      // A reset while the page was in flight: drop it." },
    ] }] },
    { path: "web/src/state/agents.test.ts", isNew: true, isDeleted: false, additions: 24, deletions: 0, status: "ok", hunks: [{ oldStart: 0, oldCount: 0, newStart: 1, newCount: 4, lines: [{ type: "add", content: 'import { describe, expect, it } from "vitest";' }, { type: "add", content: "" }, { type: "add", content: 'describe("loadTimeline", () => {' }, { type: "add", content: '  it("survives a partial entry", () => {});' }] }] },
    { path: "docs/logo.png", isNew: true, isDeleted: false, additions: 0, deletions: 0, status: "binary", hunks: [] },
  ],
};

const TREE = {
  "": [{ name: "web", path: "web", kind: "directory", size: 0 }, { name: "docs", path: "docs", kind: "directory", size: 0 }, { name: "Cargo.toml", path: "Cargo.toml", kind: "file", size: 812 }, { name: "README.md", path: "README.md", kind: "file", size: 1604 }],
  web: [{ name: "src", path: "web/src", kind: "directory", size: 0 }, { name: "package.json", path: "web/package.json", kind: "file", size: 1210 }],
};
const FILES = { "README.md": "# SecretaryIOS\n\nThe Secretary apps and server.\n\n## Build\n\n    npm ci && npm run build\n", "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\nresolver = "2"\n', "web/package.json": '{\n  "name": "secretary-web",\n  "private": true\n}\n' };

let SCHEDULES = [
  { id: "sch_1", name: "Morning build check", prompt: "Check the CI status and fix anything red.", cadence: { type: "cron", expression: "0 7 * * 1-5", timezone: "Europe/Rome" }, target: { type: "agent", agentId: "a1c2d3e4" }, status: "active", nextRunAt: new Date(now + 14 * 3600_000).toISOString(), lastRunAt: iso(60 * 10) },
  { id: "sch_2", name: "Weekly dependency bump", prompt: "Bump dependencies.", cadence: { type: "every", everyMs: 7 * 86_400_000 }, target: { type: "new-agent", config: { provider: "claude", cwd: WORKSPACES.wks_2.cwd } }, status: "paused", nextRunAt: null, lastRunAt: iso(60 * 24 * 3) },
];

const TERMINALS = [{ id: "term_1", name: "dev", title: "dev server", cwd: WORKSPACES.wks_1.cwd, workspaceId: "wks_1", agentId: null, owner: "owner", rows: 24, cols: 80, status: "running", activity: "running", ports: [{ port: 4173, host: "127.0.0.1" }] }];
const SCRIPTS = [
  { scriptName: "web", type: "service", port: 4123, lifecycle: "running", health: "healthy", terminalId: "term_1", command: "npm run dev -- --port 4123", servicePath: "/v1/agents/workspaces/wks_1/services/web/" },
  { scriptName: "test", type: "script", lifecycle: "stopped", command: "npm test" },
];

const sse = new Set();
let eventId = Date.now() * 1000;
function broadcast(body) {
  const frame = `id: ${++eventId}\ndata: ${JSON.stringify(body)}\n\n`;
  for (const res of sse) res.write(frame);
}
const stateEvent = (id) => broadcast({ type: "agent_state", agent: AGENTS[id] });
const pushRow = (id, item) => {
  const rows = ROWS[id];
  const seq = rows.length + 1;
  rows.push({ seq, timestamp: new Date().toISOString(), item, turnId: "t2" });
  broadcast({ type: "agent_stream", agentId: id, event: { type: "timeline", item, turnId: "t2" }, seq, epoch: "e1", timestamp: new Date().toISOString() });
};

function reply(id, text) {
  AGENTS[id] = { ...AGENTS[id], status: "running", updatedAt: new Date().toISOString() };
  stateEvent(id);
  const words = `Understood: "${text}". I'll take a look and report back with what I find.`.split(" ");
  words.forEach((w, i) => setTimeout(() => pushRow(id, { type: "assistant_message", text: `${w} `, messageId: "m2" }), 400 + i * 120));
  setTimeout(() => {
    AGENTS[id] = { ...AGENTS[id], status: "idle", updatedAt: new Date().toISOString() };
    stateEvent(id);
  }, 600 + words.length * 120);
}

export async function handleAgents(req, res, { path, method, url, send, readJson }) {
  if (!path.startsWith("/v1/agents")) return false;
  const rest = path.slice("/v1/agents".length) || "/";
  const err = (status, code, message) => send(res, status, { error: { code, message } });
  const gone = () => err(404, "NOT_FOUND", "Not found");
  const isPhase56 = /^\/(terminals|search|schedules|usage)/.test(rest) || /\/(prompts|scripts|services)(\/|$)/.test(rest);
  if (absent && isPhase56) return gone(), true;
  const ok = (body) => (send(res, 200, body), true);

  // The Homelab catalog (/v1/agents/catalog/*): with MOCK_CATALOG_BIN (the real `catalog` CLI on nezopt) the routes
  // answer from the live scan; without it they are absent, as against an agentd without them.
  if (rest.startsWith("/catalog")) {
    const bin = process.env.MOCK_CATALOG_BIN;
    if (!bin) return gone(), true;
    const parts = rest.split("/").filter(Boolean).map(decodeURIComponent);
    const q = url.searchParams;
    let args;
    if (parts.length === 1) args = ["tree"];
    else if (parts[1] === "components" && parts[2]) args = ["get", parts[2]];
    else if (parts[1] === "impact" && parts[2]) args = ["impact", ...parts[2].split(",")];
    else if (parts[1] === "health") args = ["health", ...(q.get("id") ? [q.get("id")] : []), ...(q.get("live") === "true" ? ["--live"] : [])];
    else if (["status", "drift", "graph", "events"].includes(parts[1])) args = [parts[1]];
    else return gone(), true;
    const { execFile } = await import("node:child_process");
    const out = await new Promise((resolve) => execFile(bin, ["--json", ...args], { maxBuffer: 64 << 20 }, (e, stdout, stderr) => resolve(e ? { e: String(stderr || e) } : { stdout })));
    if (out.e) return err(out.e.includes("no component") ? 404 : 502, out.e.includes("no component") ? "CATALOG_NOT_FOUND" : "CATALOG_UNAVAILABLE", out.e.trim().split("\n").pop()), true;
    const body = JSON.parse(out.stdout);
    if (args[0] === "tree") {
      // agentd adds the agents working in each project; the mock adds its own when one's cwd matches.
      for (const g of body.groups) for (const p of g.projects) p.agents = Object.values(AGENTS).filter((a) => p.paths.some((x) => a.cwd === x || a.cwd?.startsWith(`${x}/`))).map((a) => ({ id: a.id, shortId: a.shortId, title: a.title, provider: a.provider, status: a.status }));
    }
    return ok(body);
  }

  if (rest === "/stream" && method === "GET") {
    res.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-store", Connection: "keep-alive" });
    res.write(": ok\n\n");
    sse.add(res);
    const ping = setInterval(() => res.write(": keep-alive\n\n"), 15_000);
    req.on("close", () => {
      clearInterval(ping);
      sse.delete(res);
    });
    return true;
  }
  if (rest === "/items") return ok({ agents: Object.values(AGENTS).filter((a) => !!a.archivedAt === (url.searchParams.get("archived") === "true")).map(itemOf) });
  if (rest === "/" && method === "GET") return ok({ agents: Object.values(AGENTS) });
  if (rest === "/" && method === "POST") {
    const b = await readJson(req);
    const id = Math.random().toString(16).slice(2, 10);
    AGENTS[id] = snap(id, { title: String(b.prompt ?? "New agent").slice(0, 40), provider: b.provider, model: b.model ?? "default", status: "running", updatedAt: new Date().toISOString() });
    ROWS[id] = [];
    if (b.prompt) pushRowLater(id, b.prompt);
    return ok(AGENTS[id]);
  }
  if (rest === "/providers") return ok([
    { id: "claude", label: "Claude", defaultModeId: "default", modes: [{ id: "default", label: "Default" }, { id: "acceptEdits", label: "Accept edits" }], models: [{ id: "opus-5.5", label: "Opus 5.5", isDefault: true, isSelectable: true }, { id: "sonnet-5.5", label: "Sonnet 5.5", isSelectable: true }] },
    { id: "codex", label: "Codex", modes: [], models: [{ id: "gpt-6.1-sol", label: "GPT-6.1 Sol", isDefault: true, isSelectable: true }] },
    { id: "opencode", label: "OpenCode", modes: [], models: [] },
  ]);
  if (rest === "/permissions" && method === "GET") return ok({ permissions: PERMS });
  const pa = /^\/permissions\/([^/]+)$/.exec(rest);
  if (pa && method === "POST") {
    const b = await readJson(req);
    PERMS = PERMS.map((p) => (p.id === pa[1] ? { ...p, status: b.behavior === "allow" ? "allowed" : "denied", resolution: { behavior: b.behavior, by: "owner_device", at: new Date().toISOString() } } : p));
    broadcast({ type: "permission", permission: PERMS.find((p) => p.id === pa[1]) });
    return ok(PERMS.find((p) => p.id === pa[1]));
  }
  if (rest === "/workspaces" && method === "GET") return ok({ workspaces: Object.values(WORKSPACES) });
  if (rest === "/projects") return ok({ projects: [{ projectId: "prj_1", rootPath: "/home/nez/Projects/Startup/SecretaryIOS", name: "SecretaryIOS" }, { projectId: "prj_2", rootPath: "/home/nez/Projects/Lucina/lucina-rs", name: "lucina-rs" }] });
  if (rest === "/usage") return ok({ providers: [{ provider: "claude", agents: 14, running: 2, inputTokens: 4_800_000, cachedInputTokens: 3_900_000, outputTokens: 210_000, totalCostUsd: 38.4 }, { provider: "codex", agents: 6, running: 1, inputTokens: 1_100_000, cachedInputTokens: 600_000, outputTokens: 95_000, totalCostUsd: 9.75 }, { provider: "opencode", agents: 9, running: 0, inputTokens: 700_000, cachedInputTokens: 0, outputTokens: 60_000, totalCostUsd: 0 }], schedules: { active: 1, paused: 1, completed: 3 }, since: iso(60 * 24 * 7) });
  if (rest === "/search") {
    const q = (url.searchParams.get("q") ?? "").toLowerCase();
    return ok({ query: q, results: Object.values(AGENTS).filter((a) => `${a.title} ${a.provider}`.toLowerCase().includes(q)).map((a) => ({ agentId: a.id, title: a.title, provider: a.provider, status: a.status, cwd: a.cwd, updatedAt: a.updatedAt, score: 1, matches: [{ field: "title", snippet: a.title }] })) });
  }
  if (rest === "/schedules" && method === "GET") return ok({ schedules: SCHEDULES });
  const sc = /^\/schedules\/([^/]+)\/(pause|resume|run-once)$/.exec(rest);
  if (sc && method === "POST") {
    SCHEDULES = SCHEDULES.map((s) => (s.id === sc[1] ? { ...s, status: sc[2] === "pause" ? "paused" : sc[2] === "resume" ? "active" : s.status } : s));
    return ok(SCHEDULES.find((s) => s.id === sc[1]));
  }
  if (rest === "/terminals" && method === "GET") return ok({ terminals: TERMINALS });
  if (rest === "/terminals" && method === "POST") return ok(TERMINALS[0]);
  const ts = /^\/terminals\/([^/]+)\/stream$/.exec(rest);
  if (ts) {
    res.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-store" });
    const out = (event, data, id) => res.write(`event: ${event}\n${id ? `id: ${id}\n` : ""}data: ${JSON.stringify(data)}\n\n`);
    out("snapshot", { rows: 24, cols: 80, offset: 0, data: "\u001b[1m> secretary-web@0.1.0 dev\u001b[0m\r\n> vite --port 4123\r\n\r\n  \u001b[32mVITE v8.3.2\u001b[0m  ready in 412 ms\r\n\r\n  \u001b[32m➜\u001b[0m  Local:   http://127.0.0.1:4123/\r\n" });
    const t = setInterval(() => out("output", { b64: Buffer.from(`${new Date().toISOString().slice(11, 19)} [vite] hmr update /src/App.tsx\r\n`).toString("base64") }, String(Date.now())), 4000);
    req.on("close", () => clearInterval(t));
    return true;
  }
  if (/^\/terminals\/[^/]+\/input$/.test(rest)) return ok({ terminalId: "term_1", keysSent: 1 });
  const ws = /^\/workspaces\/([^/]+)\/(diff|files|file|scripts|services|download-token)$/.exec(rest);
  if (ws) {
    if (ws[2] === "diff") return ok(DIFF);
    if (ws[2] === "files") return ok({ path: url.searchParams.get("path") ?? "", entries: (TREE[url.searchParams.get("path") ?? ""] ?? []).map((e) => ({ ...e, modifiedAt: iso(30) })) });
    if (ws[2] === "file") {
      const p = url.searchParams.get("path") ?? "";
      return ok({ path: p, kind: "text", encoding: "utf-8", content: FILES[p] ?? "(empty)\n", size: (FILES[p] ?? "").length, modifiedAt: iso(30) });
    }
    if (ws[2] === "scripts") return ok({ workspaceId: ws[1], scripts: SCRIPTS });
    if (ws[2] === "services") return ok({ workspaceId: ws[1], services: [{ name: "web", kind: "script", scriptName: "web", port: 4123, path: "/v1/agents/workspaces/wks_1/services/web/" }, { name: "port-4173", kind: "detected", port: 4173, path: "/v1/agents/workspaces/wks_1/services/port-4173/" }] });
    return ok({ token: "tok", url: "/files/download?token=tok", expiresInSec: 60 });
  }
  const ag = /^\/([^/]+)(?:\/([a-z-]+)(?:\/([^/]+))?)?$/.exec(rest);
  if (ag) {
    const [, id, action, arg] = ag;
    const a = AGENTS[id];
    if (!a) return err(404, "AGENT_NOT_FOUND", `Agent not found: ${id}`), true;
    if (!action && method === "GET") return ok(a);
    if (!action && method === "PATCH") {
      AGENTS[id] = { ...a, title: (await readJson(req)).title };
      stateEvent(id);
      return ok(AGENTS[id]);
    }
    if (action === "timeline") {
      const after = Number(url.searchParams.get("afterSeq") ?? 0);
      const rows = (ROWS[id] ?? []).filter((r) => r.seq > after);
      return ok({ epoch: "e1", rows, nextSeq: rows.at(-1)?.seq ?? after });
    }
    if (action === "prompts") return ok({ epoch: "e1", prompts: (ROWS[id] ?? []).filter((r) => r.item.type === "user_message").map((r) => ({ seq: r.seq, timestamp: r.timestamp, preview: r.item.text.slice(0, 120) })) });
    if (action === "send") {
      const b = await readJson(req);
      pushRow(id, { type: "user_message", text: b.prompt });
      reply(id, b.prompt);
      return ok({ agentId: id, disposition: "started" });
    }
    if (action === "stop") {
      AGENTS[id] = { ...a, status: "idle" };
      stateEvent(id);
      return ok({ agentId: id, stopped: true });
    }
    if (action === "archive") {
      AGENTS[id] = { ...a, archivedAt: new Date().toISOString() };
      stateEvent(id);
      return ok({ agentId: id, archivedAt: AGENTS[id].archivedAt });
    }
    if (action === "unarchive") {
      AGENTS[id] = { ...a, archivedAt: null };
      stateEvent(id);
      return ok(AGENTS[id]);
    }
    if (action === "permissions" && arg) {
      const b = await readJson(req);
      PERMS = PERMS.map((p) => (p.id === arg && p.agentId === id ? { ...p, status: b.behavior === "allow" ? "allowed" : "denied", resolution: { behavior: b.behavior, by: "owner_device", at: new Date().toISOString() } } : p));
      broadcast({ type: "permission", permission: PERMS.find((p) => p.id === arg && p.agentId === id) });
      AGENTS[id] = { ...a, pendingPermissions: [], requiresAttention: false, attentionReason: undefined };
      stateEvent(id);
      return ok({ agentId: id, requestId: arg, behavior: b.behavior });
    }
  }
  return gone(), true;
}

function pushRowLater(id, text) {
  setTimeout(() => {
    pushRow(id, { type: "user_message", text });
    reply(id, text);
  }, 300);
}
