// Generates the Paseo reference fixtures for crates/lucina-agents/tests/paseo_fixtures.rs
// by running Paseo 0.13.2's own code on the cases below.
//
//   node crates/lucina-agents/tests/fixtures/gen/gen.mjs \
//     /home/nez/.local/lib/node_modules/@getpaseo/cli/node_modules/@getpaseo \
//     crates/lucina-agents/tests/fixtures
//
// Object keys in tool inputs are written in sorted order: lucina-agents stores JSON
// objects sorted (serde_json without preserve_order), Paseo keeps insertion order.
import { writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { join } from "node:path";

const [root, outDir] = process.argv.slice(2);
if (!root || !outDir) {
  console.error("usage: gen.mjs <@getpaseo dir> <out dir>");
  process.exit(2);
}
const load = (p) => import(pathToFileURL(join(root, p)).href);
const curator = await load("server/dist/server/server/agent/activity-curator.js");
const titles = await load("server/dist/server/server/agent/create-agent-title.js");
const buckets = await load("protocol/dist/agent-state-bucket.js");

const tool = (callId, name, detail, status = "completed", extra = {}) => ({
  type: "tool_call",
  callId,
  name,
  detail,
  status,
  error: status === "failed" ? "denied" : null,
  ...extra,
});
const unknown = (input, output = null) => ({ type: "unknown", input, output });
const longText = (n, c = "x") => c.repeat(n);

const timelines = {
  empty: [],
  chat: [
    { type: "user_message", text: "  Fix the scroll anchor  " },
    { type: "assistant_message", text: "Looking" },
    { type: "assistant_message", text: " at it." },
  ],
  reasoning_chunks: [
    { type: "reasoning", text: "First " },
    { type: "reasoning", text: "thought" },
    { type: "assistant_message", text: "Done." },
  ],
  tools_canonical: [
    tool("c1", "Bash", { type: "shell", command: "cargo test\n  -p foo", exitCode: 0 }),
    tool("c2", "Read", { type: "read", filePath: "/w/src/lib.rs" }),
    tool("c3", "Edit", { type: "edit", filePath: "/w/src/lib.rs", oldString: "a", newString: "b" }),
    tool("c4", "Write", { type: "write", filePath: "/w/new.rs" }),
    tool("c5", "Grep", { type: "search", query: "TODO", toolName: "grep" }),
    tool("c6", "WebFetch", { type: "fetch", url: "https://example.com" }),
    tool("c7", "setup", { type: "worktree_setup", worktreePath: "/w", branchName: "agent/x", log: "", commands: [] }),
    tool("c8", "Task", { type: "sub_agent", subAgentType: "Explore", description: "find the bug", log: "  found it  " }),
    tool("c9", "terminal", { type: "plain_text", label: "npm run dev" }),
    tool("c10", "ExitPlanMode", { type: "plan", text: "1. do it" }),
    tool("c11", "some_custom-tool", unknown({ a: 1 })),
    tool("c12", "Task", unknown({}), "running", { metadata: { subAgentActivity: "reading files" } }),
    tool("c13", "Thinking", unknown(null)),
    tool("c14", "Bash", { type: "shell", command: "rm -rf /" }, "failed"),
  ],
  tool_lifecycle: [
    tool("t1", "Bash", unknown({ command: "ls" }), "running"),
    { type: "assistant_message", text: "while it runs" },
    tool("t1", "Bash", { type: "shell", command: "ls", output: "a" }, "completed"),
  ],
  external_tools: [
    tool("e1", "mcp__github__create_issue", unknown({ body: longText(500), title: "bug" })),
    tool("e2", "mcp__lucina__shopping_add", unknown({ name: "milk" })),
    tool("e3", "speak", unknown({ text: "hello" })),
    tool("e4", "mcp__github__get_me", { type: "plan", text: "x" }),
  ],
  paseo_tools: [
    tool("p1", "mcp__paseo__create_agent", unknown({ initialPrompt: "Refactor the parser\nand add tests", title: "" })),
    tool("p2", "mcp__paseo__send_agent_prompt", unknown({ agentId: "a1", prompt: longText(300, "y") })),
    tool("p3", "mcp__paseo__get_agent_status", unknown({ agentId: "abc123" })),
    tool("p4", "paseo.list_agents", unknown({})),
    tool("p5", "mcp__paseo__create_workspace", unknown({ path: "/home/nez/Projects/x/" })),
    tool("p6", "mcp__paseo__browser_resize", unknown({ height: 600, width: 800 })),
    tool("p7", "mcp__paseo__browser_upload", unknown({ filePaths: ["a.png", "b.png", "c.png"] })),
    tool("p8", "mcp__paseo__future_tool_name", unknown({})),
  ],
  agent_messages: [
    tool("m1", "notify", unknown({}), "completed", {
      agentMessage: { event: "finished", sender: { id: "child-1", title: "Child agent" }, text: "  All done.  " },
    }),
    tool("m2", "notify", unknown({}), "completed", {
      agentMessage: { event: "permission-required", sender: { id: "child-2" }, text: "" },
    }),
  ],
  misc: [
    { type: "todo", items: [{ text: "write tests", completed: true }, { text: "ship", completed: false }] },
    { type: "error", message: "rate limited" },
    { type: "compaction", status: "completed", trigger: "auto" },
    { type: "notification", level: "info", message: "ignored by the curator" },
    { type: "user_message", text: "" },
    { type: "assistant_message", text: "   " },
  ],
  long_summary: [
    tool("l1", "Bash", { type: "shell", command: longText(250, "z") }),
    tool("l2", "Bash", { type: "shell", command: "a  b﻿c" }),
  ],
  unicode_cut: [
    tool("u1", "mcp__x__y", unknown({ s: longText(390) + "😀😀😀😀😀😀" })),
    tool("u2", "Bash", { type: "shell", command: longText(196) + "😀😀😀" }),
  ],
  many: Array.from({ length: 7 }, (_, i) => ({ type: "user_message", text: `prompt ${i}` })),
};

const curatorCases = [];
for (const [name, items] of Object.entries(timelines)) {
  for (const limit of [undefined, 0, 1, 2, 5]) {
    curatorCases.push({ name, limit: limit ?? null, items, expected: curator.summarizeAgentActivity(items, limit) });
  }
  curatorCases.push({
    name: `${name}/curate-labelled`,
    limit: null,
    items,
    options: { labelAssistantMessages: true, includeKinds: ["user_message", "assistant_message", "tool_call"], includeExternalToolInput: false, maxItems: 3 },
    expectedCurate: curator.curateAgentActivity(items, { labelAssistantMessages: true, includeKinds: ["user_message", "assistant_message", "tool_call"], includeExternalToolInput: false, maxItems: 3 }),
  });
}

const titleInputs = [
  [null, "Fix the bug"],
  [null, "\n\n   \r\n  first line here  \nsecond"],
  [null, "   "],
  [null, longText(80, "w")],
  [null, "a  \t b　c"],
  ["  Explicit title  ", "prompt"],
  ["   ", "fallback prompt"],
  [null, null],
  [null, longText(58, "v") + " 😀😀"],
  [null, "ends with spaces at the cut" + " ".repeat(40) + "tail"],
];
const titleCases = titleInputs.map(([configTitle, initialPrompt]) => ({
  configTitle,
  initialPrompt,
  expected: titles.resolveCreateAgentTitles({ configTitle, initialPrompt }),
}));

const bucketCases = [];
for (const status of ["initializing", "idle", "running", "error", "closed"]) {
  for (const pendingPermissionCount of [0, 2]) {
    for (const requiresAttention of [false, true]) {
      for (const attentionReason of [null, "finished", "error", "permission"]) {
        const input = { status, pendingPermissionCount, requiresAttention, attentionReason };
        bucketCases.push({
          input,
          bucket: buckets.deriveAgentStateBucket(input),
          priority: buckets.getAgentStatusPriority(input),
        });
      }
    }
  }
}

const attention = await load("protocol/dist/agent-attention-notification.js");
const messages = [
  null,
  "",
  "   \n\t ",
  "Done.",
  "## Summary\n\n- **Fixed** the `scroll` anchor in [the list](https://example.com/a_(b))\n- Added _tests_ and ~~removed~~ code\n> quoted *line*\n\n```rust\nfn main() {}\n```\n---\n1. one\n<https://example.com>",
  "![logo](img.png) and __bold__ text\r\nnext line　wide",
  "word ".repeat(60),
  "x".repeat(219) + "😀😀",
  " ".repeat(10) + "y".repeat(300),
  "~~~\ncode\n~~~\n***\n  3. item\n####### seven hashes",
];
const permissions = [
  null,
  { id: "p1", provider: "claude", name: "Bash", kind: "tool", title: "Run npm test", description: "Runs the tests" },
  { id: "p2", provider: "claude", name: "Bash", kind: "tool", title: "  Same  ", description: "Same" },
  { id: "p3", provider: "codex", name: "apply_patch", kind: "tool", input: { a: "x".repeat(300), b: 1 } },
  { id: "p4", provider: "mock", name: "MockQuestions", kind: "question", metadata: { source: "mock_questions" } },
  { id: "p5", provider: "mock", name: "  ", kind: "plan" },
  { id: "p6", provider: "mock", name: "ExitPlanMode", kind: "plan", description: "**Plan**: do `x`" },
];
const attentionCases = [];
for (const reason of ["finished", "permission", "error"]) {
  for (const assistantMessage of messages) {
    for (const permissionRequest of permissions) {
      if (reason === "finished" && permissionRequest) continue;
      if (reason !== "finished" && assistantMessage) continue;
      const payload = attention.buildAgentAttentionNotificationPayload({
        reason, serverId: "s", agentId: "a", assistantMessage, permissionRequest,
      });
      attentionCases.push({ reason, assistantMessage, permissionRequest, title: payload.title, body: payload.body });
    }
  }
}
const latestTimelines = [
  [],
  [{ type: "user_message", text: "hi" }],
  [{ type: "assistant_message", text: "a" }, { type: "assistant_message", text: "b" }],
  [{ type: "assistant_message", text: "old" }, { type: "tool_call", callId: "c", name: "Bash", detail: { type: "shell", command: "ls" }, status: "completed", error: null }, { type: "assistant_message", text: "new " }, { type: "assistant_message", text: "end" }, { type: "user_message", text: "after" }],
];
const latestCases = latestTimelines.map((items) => ({ items, expected: attention.findLatestAssistantMessageFromTimeline(items) }));

const slug = await load("protocol/dist/branch-slug.js");
const slugInputs = ["Fix the login redirect bug", "  --Ünïcode__x  ", "", "---", "ALL CAPS 123", "İstanbul straße", "a".repeat(60), "word-".repeat(12), "abcdefghij-abcdefghij-abcdefghij-abcdefghij-abcdefghij-x", "x".repeat(24) + "-" + "y".repeat(40), "PR #2638: Playwright specs!", "feat/x", "emoji 😀 here"];
const slugCases = slugInputs.map((input) => ({ input, slug: slug.slugify(input) }));
const validateInputs = ["feat/x", "", "Feat", "a--b", "-a", "a-", "a b", "x".repeat(101), "x".repeat(100), "agent/fix-login-2", "a_b"];
const validateCases = validateInputs.map((input) => ({ input, ...slug.validateBranchSlug(input) }));

// A cut through a surrogate pair leaves a lone surrogate, which Node prints as U+FFFD
// and serde_json refuses to parse; store it as U+FFFD.
const wellFormed = (_key, v) => (typeof v === "string" ? v.toWellFormed() : v);
const write = (file, value) =>
  writeFileSync(join(outDir, file), JSON.stringify(value, wellFormed, 1) + "\n");
write("curator.json", curatorCases);
write("titles.json", titleCases);
write("buckets.json", bucketCases);
write("attention.json", { payloads: attentionCases, latest: latestCases });
write("branch-slug.json", { slugify: slugCases, validate: validateCases });
console.log(`curator ${curatorCases.length}, titles ${titleCases.length}, buckets ${bucketCases.length}, attention ${attentionCases.length}+${latestCases.length}`);
