// Generates the phase-6 Paseo reference fixtures (schedules, envelopes, search) for
// crates/lucina-agents/tests/paseo_orchestration.rs and
// crates/lucina-agentd/tests/next_run.rs, by running Paseo 0.13.2's own code:
//
//   node crates/lucina-agents/tests/fixtures/gen/gen-orchestration.mjs \
//     /home/nez/.local/lib/node_modules/@getpaseo/cli/node_modules/@getpaseo \
//     crates/lucina-agents/tests/fixtures
import { writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { join } from "node:path";

const [root, outDir] = process.argv.slice(2);
if (!root || !outDir) {
  console.error("usage: gen-orchestration.mjs <@getpaseo dir> <out dir>");
  process.exit(2);
}
const load = (p) => import(pathToFileURL(join(root, p)).href);
const cronExpr = await load("protocol/dist/schedule/cron-expression.js");
const cron = await load("server/dist/server/server/schedule/cron.js");
const envelope = await load("server/dist/server/server/agent/agent-messages/envelope.js");
const notification = await load("server/dist/server/server/agent/finish-subscriptions/internal/notification.js");
const textMatch = await load("protocol/dist/search/text-match.js");
const write = (name, value) => writeFileSync(join(outDir, name), JSON.stringify(value, null, 2) + "\n");

// -- cron parsing ---------------------------------------------------------------
const expressions = [
  "* * * * *", "0 7 * * 1-5", "*/15 * * * *", "0 0 1 1 *", "5,10,15 9-17/2 * * *", " 0  7 * *  * ",
  "* * * *", "* * * * * *", "", "60 * * * *", "* 24 * * *", "* * 0 * *", "* * 32 * *", "* * * 13 *",
  "* * * * 7", "*/0 * * * *", "*/ * * * *", "*/05 * * * *", "1-2-3 * * * *", "9-7 * * * *", "a * * * *",
  "1,,2 * * * *", "1/2/3 * * * *", "-1 * * * *", "0-59/30 */6 1,15 */3 0", "00 07 * * *", "5/2 * * * *",
];
write("cron-parse.json", expressions.map((expression) => ({ expression, error: cronExpr.validateCronExpression(expression) })));

// -- next run ---------------------------------------------------------------------
const cadences = [
  { type: "cron", expression: "0 7 * * 1-5" },
  { type: "cron", expression: "0 7 * * 1-5", timezone: "Europe/Rome" },
  { type: "cron", expression: "30 2 * * *", timezone: "Europe/Rome" },
  { type: "cron", expression: "0 3 * * *", timezone: "Europe/Rome" },
  { type: "cron", expression: "*/20 * * * *", timezone: "America/New_York" },
  { type: "cron", expression: "0 1 * * *", timezone: "America/New_York" },
  { type: "cron", expression: "15 9 1 * *", timezone: "Asia/Kolkata" },
  { type: "cron", expression: "0 2 * * 0", timezone: "Australia/Lord_Howe" },
  { type: "cron", expression: "0 0 29 2 *" },
  { type: "cron", expression: "59 23 31 12 *", timezone: "Pacific/Kiritimati" },
  { type: "cron", expression: "* * * * *" },
  { type: "every", everyMs: 300000 },
  { type: "every", everyMs: 3600000 },
];
const afters = [
  "2026-10-28T10:43:12.345Z", "2026-03-29T00:59:59.999Z", "2026-03-29T01:30:00.000Z", "2026-10-25T00:30:00.000Z",
  "2026-10-25T01:00:00.000Z", "2026-11-01T05:30:00.000Z", "2026-03-08T06:59:00.000Z", "2026-12-31T23:59:30.000Z",
  "2027-04-04T14:00:00.000Z", "2026-10-03T15:00:00.000Z", "2026-02-28T12:00:00.000Z",
];
const nextRun = [];
for (const cadence of cadences) {
  for (const after of afters) {
    let next = null;
    let error = null;
    try {
      next = cron.computeNextRunAt(cadence, new Date(after)).toISOString();
    } catch (e) {
      error = e.message;
    }
    nextRun.push({ cadence, after, next, error });
  }
}
write("next-run.json", nextRun);

// -- envelopes --------------------------------------------------------------------
const messages = [
  { id: "m1", source: null, text: "hello" },
  { id: "m2", source: { kind: "agent-message", agentId: "a1" }, text: "please check <this> & that" },
  { id: "m\"3", source: { kind: "agent-message", agentId: "a'2", serverId: "nezopt", title: "Fix \"it\"" }, text: "" },
  { id: "m4", source: { kind: "agent-notification", agentId: "c1", title: "Child", event: "finished" }, text: "Agent c1 (Child) finished." },
  { id: "m5", source: { kind: "agent-notification", agentId: "c1", event: "permission-required" }, text: "line1\nline2" },
  { id: "m6", source: { kind: "agent-notification", agentId: "c1", event: "errored" }, text: "x" },
  { id: "m7", source: { kind: "agent-notification", agentId: "c1", event: "closed" }, text: "y" },
];
const formatted = messages.map((message) => ({ message, text: envelope.formatAgentMessage(message) }));
const parseInputs = [
  ...formatted.map((f) => f.text),
  formatted[0].text + "\n\n" + formatted[1].text,
  formatted[0].text + "\n" + formatted[3].text + "\n\n\n" + formatted[4].text,
  formatted[0].text + " trailing",
  "prefix " + formatted[0].text,
  "plain prompt",
  '<paseo-system version="2" kind="user-message" message-id="x">\nhi\n</paseo-system>',
  '<paseo-system version="1" kind="user-message" message-id="x" source-agent-id="a">\nhi\n</paseo-system>',
  '<paseo-system version="1" kind="agent-message" message-id="x">\nhi\n</paseo-system>',
  '<paseo-system version="1" kind="agent-message" source-agent-id="" message-id="x">\nhi\n</paseo-system>',
  '<paseo-system version="1" kind="agent-message" source-agent-id="a" message-id="x" source-server-id="">\nhi\n</paseo-system>',
  '<paseo-system version="1" kind="agent-notification" source-agent-id="a" message-id="x" event="bogus">\nhi\n</paseo-system>',
  '<paseo-system version="1" kind="user-message" message-id="x" message-id="y">\nhi\n</paseo-system>',
  '<paseo-system version="1" kind="user-message" message-id="x">\na <b> c\n</paseo-system>',
  '<paseo-system version="1" kind="user-message" message-id="x">\na & c\n</paseo-system>',
  '<paseo-system version="1" kind="user-message" message-id="">\nhi\n</paseo-system>',
  '<paseo-system version="1"kind="user-message" message-id="x">\nhi\n</paseo-system>',
  '<paseo-system version="1" kind="user-message" message-id="x">\n\n</paseo-system>',
  '<paseo-system version="1" kind="user-message" message-id="x">hi</paseo-system>',
  "<paseo-system>\nSchedule fired.\n</paseo-system>",
  "<paseo-system>\n\n</paseo-system>",
  "<paseo-system>\n</paseo-system>",
];
const parsed = parseInputs.map((text) => ({
  text,
  messages: envelope.parseAgentMessages(text),
  systemInjected: envelope.isSystemInjectedEnvelope(text),
  projected: envelope.projectAgentMessages({ type: "user_message", text }),
}));
const request = {
  id: "perm-1",
  provider: "claude",
  name: "Bash",
  kind: "tool",
  title: "Run tests",
  input: { command: "cargo test" },
};
const reasons = ["finished", "errored", "needs permission", "was closed", "is unavailable", "was canceled"];
const longMessage = "é".repeat(3990) + "x".repeat(30);
const finish = [];
for (const reason of reasons) {
  for (const [lastAssistantMessage, serverId, permissionRequest] of [
    [null, undefined, undefined],
    ["  All tests pass.\n", undefined, request],
    [longMessage, "nezopt", request],
    ["", "nezopt", undefined],
  ]) {
    const params = { childAgentId: "c-1", serverId, title: "Child task", reason, lastAssistantMessage, permissionRequest };
    finish.push({ params, event: notification.finishNotificationEvents[reason], body: notification.formatFinishNotificationBody(params) });
  }
}
write("envelopes.json", { formatted, parsed, finish, systemNotification: envelope.formatSystemNotificationPrompt("Schedule \"x\" fired (id=ab, run=r1).\nCheck CI") });

// -- text match -------------------------------------------------------------------
const fields = [
  ["Fix the scroll anchor", "lucina-agents", "agent/fix-scroll", "SecretaryIOS"],
  ["Configuration loader refactor", "", "main", "lucina"],
  ["Add Paseo schedules", "agents-p6", "agents-p6", "SecretaryIOS"],
  ["naïve café test", "", "", "x"],
  ["", "", "", ""],
];
const queries = [
  "", "fix", "FIX", "scroll anchor", "scrol", "ancor", "scorll", "confug", "configuraton", "loadr", "refcator",
  "agents p6", "p6", "paseo sched", "sio", "cafe", "café", "naive", "zzz", "fix zzz", "secretaryios", "a", "anchr",
  "fx", "schedules", "sched", "ule", "  spaced   query ", "agent/fix", "fix-scroll", "lucina", "lucna", "luicna",
];
const scores = [];
for (const f of fields) {
  for (const q of queries) {
    scores.push({ query: q, fields: f, typo: textMatch.scoreTextFields(q, f, { typoTolerant: true }), exact: textMatch.scoreTextFields(q, f) });
  }
}
write("text-match.json", scores);
console.error(`wrote cron-parse (${expressions.length}), next-run (${nextRun.length}), envelopes, text-match (${scores.length})`);
