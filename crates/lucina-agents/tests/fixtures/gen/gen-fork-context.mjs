// Generates the fork-context reference fixture (parity row A16) for
// crates/lucina-agents/tests/paseo_fork_context.rs by running Paseo 0.13.2's own
// `buildAgentForkContextAttachment` (what `agent.fork_context.request` answers with):
//
//   node crates/lucina-agents/tests/fixtures/gen/gen-fork-context.mjs \
//     /home/nez/.local/lib/node_modules/@getpaseo/cli/node_modules/@getpaseo \
//     crates/lucina-agents/tests/fixtures
//
// Object keys in tool inputs are written in sorted order (see gen.mjs).
import { writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { join } from "node:path";

const [root, outDir] = process.argv.slice(2);
if (!root || !outDir) {
  console.error("usage: gen-fork-context.mjs <@getpaseo dir> <out dir>");
  process.exit(2);
}
const load = (p) => import(pathToFileURL(join(root, p)).href);
const curator = await load("server/dist/server/server/agent/activity-curator.js");

const user = (text) => ({ type: "user_message", text });
const assistant = (text, messageId) => ({ type: "assistant_message", text, ...(messageId ? { messageId } : {}) });
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
// Long texts are written as {"$cat": [literal | {"$rep": [unit, n]}, ...]} in the fixture (the Rust test
// expands them), so the file stays small; Paseo gets the expanded strings.
const cat = (...parts) => ({ $cat: parts });
const rep = (unit, n) => ({ $rep: [unit, Math.floor(n)] });
const expand = (v) => {
  if (Array.isArray(v)) return v.map(expand);
  if (v && typeof v === "object") {
    if ("$cat" in v) return v.$cat.map((p) => (typeof p === "string" ? p : p.$rep[0].repeat(p.$rep[1]))).join("");
    return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, expand(x)]));
  }
  return v;
};
// FNV-1a 64 over the UTF-8 bytes, to compare long texts without storing them.
const fnv = (text) => {
  let h = 0xcbf29ce484222325n;
  for (const b of Buffer.from(text, "utf8")) h = ((h ^ BigInt(b)) * 0x100000001b3n) & 0xffffffffffffffffn;
  return h.toString(16).padStart(16, "0");
};
const digest = (text) => ({ len: text.length, fnv: fnv(text), head: text.slice(0, 300), tail: text.slice(-300) });
// Rows numbered from 1, all in turn t1 unless an item says otherwise.
const rowsOf = (items) =>
  items.map((item, i) => ({ seq: i + 1, timestamp: "2026-10-28T10:00:00.000Z", turnId: "t1", item }));

const chat = [
  user("  Fix the scroll anchor  "),
  assistant("Looking", "m1"),
  assistant(" at it.", "m1"),
  user("and the test"),
  assistant("Done.", "m2"),
  assistant("One more thing.", "m3"),
];
const rich = [
  user("refactor the parser"),
  { type: "reasoning", text: "thinking about it" },
  tool("c1", "Bash", { type: "shell", command: "cargo test\n  -p foo", exitCode: 0 }),
  tool("c2", "Read", { type: "read", filePath: "/w/src/lib.rs" }),
  tool("c3", "mcp__github__create_issue", unknown({ body: "x".repeat(500), title: "bug" })),
  { type: "todo", items: [{ text: "ship", completed: false }] },
  { type: "notification", level: "info", message: "ignored" },
  { type: "error", message: "rate limited" },
  { type: "compaction", status: "completed", trigger: "auto" },
  tool("m1", "notify", unknown({}), "completed", {
    agentMessage: { event: "finished", sender: { id: "child-1", title: "Child agent" }, text: "  All done.  " },
  }),
  tool("c8", "Task", { type: "sub_agent", subAgentType: "Explore", description: "find the bug", log: "  found it  " }),
  assistant("Parser refactored.", "m9"),
  { type: "reasoning", text: "wrap up" },
];
// A call that starts, is interrupted by a message, then completes: one projected entry spanning seq 2..4.
const spanning = [
  user("go"),
  tool("t1", "Bash", unknown({ command: "ls" }), "running"),
  assistant("while it runs", "m1"),
  tool("t1", "Bash", { type: "shell", command: "ls", output: "a" }, "completed"),
  assistant("after", "m2"),
];
const duplicates = [user("a"), assistant("first", "dup"), user("b"), assistant("second", "dup"), user("c"), assistant("third", "m3")];

// Overhead of the wrapper with no title and no directory: an entry of length k makes the whole
// text overhead + k long.
const overhead = curator.buildAgentForkContextAttachment({ rows: rowsOf([user("")]), cursorBoundary: null, boundaryMessageId: null, agentTitle: null, cwd: "/w" }).attachment.text.length - "[User] ".length;
const userOfExactly = (total) => user(cat(rep("a", total - overhead)));

const lines = (n, f) => Array.from({ length: n }, (_, i) => f(i));
const long = lines(80, (i) => [user(cat(`question ${i} é漢 `, rep("q", 1400))), assistant(cat(`answer ${i} 😀😀 `, rep("a", 300)), `m${i}`)]).flat();
// A repeating digit pattern, so a wrong cut shows; and a repeating run of two-byte and three-byte characters.
const pattern = (n) => cat(rep("0123456789", n / 10));
const unicode = (n) => cat(rep("éa漢ñ", n / 4), "é");

const cases = [];
const add = (name, items, opts = {}) => {
  const specRows = rowsOf(items);
  const rows = expand(specRows);
  const epoch = "ep1";
  const boundaryCursor = opts.cursor === undefined ? null : opts.cursor;
  const boundaryMessageId = opts.messageId ?? null;
  const input = {
    rows,
    cursorBoundary: boundaryCursor ? { timelineEpoch: epoch, cursor: boundaryCursor } : null,
    boundaryMessageId: boundaryMessageId ?? undefined,
    agentTitle: opts.title === undefined ? null : opts.title,
    cwd: opts.cwd ?? "/home/nez/Projects/x",
  };
  const c = { name, epoch, rows: specRows, boundaryCursor, boundaryMessageId, agentTitle: input.agentTitle, cwd: input.cwd };
  try {
    c.expected = curator.buildAgentForkContextAttachment(input);
    if (c.expected.attachment.text.length > 20000) {
      c.expected.textDigest = digest(c.expected.attachment.text);
      delete c.expected.attachment.text;
    }
  } catch (e) {
    c.error = e.message;
  }
  cases.push(c);
};

add("empty", []);
add("chat", chat);
add("chat/title+cwd", chat, { title: "  Scroll anchor bug  ", cwd: " /w/scroll " });
add("chat/long-metadata", chat, { title: "T".repeat(1500), cwd: "/" + "d".repeat(1500) });
add("chat/blank-metadata", chat, { title: "   ", cwd: "  " });
add("chat/cursor-after-merged-answer", chat, { cursor: { epoch: "ep1", seq: 3 } });
add("chat/cursor-at-user", chat, { cursor: { epoch: "ep1", seq: 4 } });
add("chat/cursor-last", chat, { cursor: { epoch: "ep1", seq: 6 } });
add("chat/cursor-inside-merge", chat, { cursor: { epoch: "ep1", seq: 2 } });
add("chat/cursor-missing", chat, { cursor: { epoch: "ep1", seq: 99 } });
add("chat/cursor-stale-epoch", chat, { cursor: { epoch: "ep0", seq: 3 } });
add("chat/message-id", chat, { messageId: "m2" });
add("chat/message-id-padded", chat, { messageId: " m1 " });
add("chat/message-id-merged", chat, { messageId: "m1" });
add("chat/message-id-gone", chat, { messageId: "zzz" });
add("chat/message-id-blank", chat, { messageId: "  " });
add("chat/cursor-wins-over-message-id", chat, { cursor: { epoch: "ep1", seq: 3 }, messageId: "m2" });
add("chat/cursor-stale-with-message-id", chat, { cursor: { epoch: "ep0", seq: 3 }, messageId: "m2" });
add("rich", rich);
add("rich/message-id", rich, { messageId: "m9" });
add("rich/cursor-mid", rich, { cursor: { epoch: "ep1", seq: 3 } });
add("spanning/cursor-at-message", spanning, { cursor: { epoch: "ep1", seq: 3 } });
add("spanning/message-id", spanning, { messageId: "m1" });
add("spanning/cursor-at-call-end", spanning, { cursor: { epoch: "ep1", seq: 4 } });
add("spanning/cursor-after", spanning, { cursor: { epoch: "ep1", seq: 5 } });
add("duplicates/message-id-last-wins", duplicates, { messageId: "dup" });
add("long/many-entries", long, { title: "Long chat" });
add("long/many-entries/message-id", long, { messageId: "m40", title: "Long chat" });
add("long/one-huge-entry", [user(pattern(150000))]);
add("long/one-huge-entry-after-small", [user("small"), assistant(pattern(120000), "m1")], { title: "t" });
add("long/one-huge-unicode-entry", [user(unicode(130001))]);
add("long/exact-fit", [userOfExactly(100000)]);
add("long/one-over", [userOfExactly(100001)]);
add("long/two-entries-exact", [user("first"), (() => { const k = 100000 - overhead - "[User] first".length - 1 - "[Assistant] ".length; return assistant(cat(rep("b", k)), "m1"); })()]);
add("long/two-entries-one-over", [user("first"), (() => { const k = 100000 - overhead - "[User] first".length - 1 - "[Assistant] ".length + 1; return assistant(cat(rep("b", k)), "m1"); })()]);

writeFileSync(join(outDir, "fork-context.json"), JSON.stringify(cases) + "\n");
console.log(`${cases.length} cases, overhead ${overhead}`);
