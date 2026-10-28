import { describe, expect, it } from "vitest";
import type { TimelineRow } from "../api/agents";
import { agentNotificationRoute, parseAgentsPath } from "./agentsRoute";
import { ansiToText, bucketOf, mergeRows, permissionText, projectTimeline, toolLine } from "./agentsTimeline";

const row = (seq: number, item: TimelineRow["item"], turnId = "t1"): TimelineRow => ({ seq, timestamp: "2026-10-28T12:00:00Z", item, turnId });

describe("projectTimeline", () => {
  it("joins streamed assistant chunks of one turn and keeps other turns apart", () => {
    const out = projectTimeline([
      row(1, { type: "user_message", text: "hi" }),
      row(2, { type: "assistant_message", text: "Hel" }),
      row(3, { type: "assistant_message", text: "lo" }),
      row(4, { type: "assistant_message", text: "next" }, "t2"),
    ]);
    expect(out.map((e) => e.item.text)).toEqual(["hi", "Hello", "next"]);
    expect(out[1]).toMatchObject({ seqStart: 2, seqEnd: 3 });
  });

  it("does not join chunks that name different message ids", () => {
    const out = projectTimeline([
      row(1, { type: "assistant_message", text: "a", messageId: "m1" }),
      row(2, { type: "assistant_message", text: "b", messageId: "m2" }),
    ]);
    expect(out).toHaveLength(2);
  });

  it("joins reasoning chunks", () => {
    const out = projectTimeline([row(1, { type: "reasoning", text: "think " }), row(2, { type: "reasoning", text: "more" })]);
    expect(out).toHaveLength(1);
    expect(out[0]!.item.text).toBe("think more");
  });

  it("folds tool call updates into the first row, a known detail beating unknown", () => {
    const out = projectTimeline([
      row(1, { type: "tool_call", callId: "c1", name: "Bash", status: "running", detail: { type: "shell", command: "ls" } }),
      row(2, { type: "assistant_message", text: "ok" }),
      row(3, { type: "tool_call", callId: "c1", name: "Bash", status: "completed", detail: { type: "unknown" } }),
    ]);
    expect(out).toHaveLength(2);
    expect(out[0]!.item).toMatchObject({ status: "completed", detail: { type: "shell", command: "ls" } });
    expect(toolLine(out[0]!.item)).toMatchObject({ kind: "shell", title: "Shell", summary: "ls" });
  });
});

describe("helpers", () => {
  it("mergeRows drops what is already there", () => {
    const have = [row(1, { type: "error", message: "x" }), row(2, { type: "error", message: "y" })];
    expect(mergeRows(have, [row(2, { type: "error", message: "y" })])).toBe(have);
    expect(mergeRows(have, [row(4, { type: "error", message: "z" }), row(3, { type: "error", message: "w" })]).map((r) => r.seq)).toEqual([1, 2, 3, 4]);
  });

  it("bucketOf follows the daemon's rules", () => {
    const base = { status: "idle" as const, pendingPermissions: [], requiresAttention: false };
    expect(bucketOf(base)).toBe("done");
    expect(bucketOf({ ...base, status: "running" })).toBe("running");
    expect(bucketOf({ ...base, status: "error" })).toBe("failed");
    expect(bucketOf({ ...base, requiresAttention: true })).toBe("attention");
    expect(bucketOf({ ...base, status: "running", attentionReason: "permission" })).toBe("needs_input");
  });

  it("ansiToText drops escapes and applies carriage returns", () => {
    expect(ansiToText("\u001b[32mgreen\u001b[0m\r\nline\rover\u001b]0;title\u0007!")).toBe("green\nover!");
  });

  it("permissionText prefers the shell command", () => {
    expect(permissionText({ command: "npm test" })).toBe("npm test");
    expect(permissionText({ file: "a" })).toContain('"file": "a"');
  });
});

describe("routes", () => {
  it("parses the agents paths", () => {
    expect(parseAgentsPath("/agents")).toEqual({ page: "list" });
    expect(parseAgentsPath("/agents/new")).toEqual({ page: "new" });
    expect(parseAgentsPath("/agents/permissions")).toEqual({ page: "permissions" });
    expect(parseAgentsPath("/agents/permissions/abc")).toEqual({ page: "permissions", id: "abc" });
    expect(parseAgentsPath("/agents/a1")).toEqual({ page: "agent", id: "a1", tab: "chat" });
    expect(parseAgentsPath("/agents/a1/changes")).toEqual({ page: "agent", id: "a1", tab: "changes" });
    expect(parseAgentsPath("/agents/a1/nonsense")).toEqual({ page: "agent", id: "a1", tab: "chat" });
  });

  it("routes an agent notification to its agent, with or without the category in the type", () => {
    expect(agentNotificationRoute({ category: "agent", route: "/", data: { agentId: "a1" } })).toBe("/agents/a1");
    expect(agentNotificationRoute({ category: "agent", route: "/agents/permissions/p1" })).toBe("/agents/permissions/p1");
    expect(agentNotificationRoute({ category: "agent", route: "/elsewhere", data: { agentId: "../x" } })).toBeNull();
    expect(agentNotificationRoute({ category: "reminder", route: "/", data: { agentId: "a1" } })).toBeNull();
  });
});
