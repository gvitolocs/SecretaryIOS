import { Route } from "@solidjs/router";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { ToolSteps } from "../components/ToolSteps";
import type { ChatMessage } from "../state/chat";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import ChatView from "../views/Chat";
import { chatRows, isToolRun, stepText, toolName, toolSummary } from "./toolSteps";

const msg = (key: string, role: "user" | "lucina", text: string, meta?: ChatMessage["meta"]): ChatMessage => ({
  key,
  role,
  text,
  createdAt: Number(key.replace(/\D/g, "")) * 1000,
  channel: "web",
  ...(meta ? { meta } : {}),
});
const step = (key: string, tool: string, ok = true) =>
  msg(key, "lucina", `🔧 ${tool} {"action":"list"} → ${ok ? "ok" : "error"}`, { kind: "tool", tool, ok });

describe("tool steps", () => {
  it("folds each run of consecutive steps into one row between messages", () => {
    const rows = chatRows([
      msg("1", "user", "Sort my university folder"),
      step("2", "drive"),
      step("3", "drive"),
      step("4", "drive", false),
      step("5", "media"),
      msg("6", "lucina", "Done: three folders."),
      step("7", "drive"),
    ]);
    expect(rows.map((r) => (isToolRun(r) ? "tools" : r.key))).toEqual(["1", "tools", "6", "tools"]);
    const first = rows[1]!;
    expect(isToolRun(first) && first.steps.map((s) => s.key)).toEqual(["2", "3", "4", "5"]);
  });

  it("keeps an unchanged run the same object, and a message row is the message itself", () => {
    const cache = new Map();
    const messages = [msg("1", "user", "hi"), step("2", "drive"), step("3", "drive")];
    const a = chatRows(messages, cache);
    const b = chatRows([...messages], cache);
    expect(b[1]).toBe(a[1]);
    expect(b[0]).toBe(messages[0]);
    const c = chatRows([...messages, step("4", "drive")], cache);
    expect(c[1]).not.toBe(a[1]);
    expect(cache.size).toBe(1);
  });

  it("names the tools in first-use order and counts failures", () => {
    expect(toolSummary([step("1", "drive"), step("2", "media", false), step("3", "drive")])).toEqual({
      tools: ["Drive", "Gallery"],
      failed: 1,
    });
    expect(toolName("web_search")).toBe("Web search");
    // An older item without meta.tool still names its tool from the text.
    expect(toolSummary([msg("1", "lucina", "🔧 calendar {} → ok", { kind: "tool" })]).tools).toEqual(["Calendar"]);
    expect(stepText(step("1", "drive"))).toBe('drive {"action":"list"} → ok');
  });

  it("shows one quiet line that opens to the steps", async () => {
    render(() => <ToolSteps steps={[step("1", "drive"), step("2", "drive"), step("3", "drive", false), step("4", "drive")]} />);
    const summary = screen.getByText("Worked on Drive · 4 steps · 1 failed");
    expect(summary.closest("details")?.open).toBe(false);
    fireEvent.click(summary);
    expect(screen.getAllByRole("listitem")).toHaveLength(4);
    expect(screen.getByText("(failed)")).toBeInTheDocument();
  });

  it("collapses a turn's tool calls in the chat instead of one bubble each", async () => {
    const items = [
      { id: "5", ts: 5000, channel: "web", role: "lucina", text: "Done: the folder is sorted." },
      ...[4, 3, 2].map((n) => ({
        id: String(n),
        ts: n * 1000,
        channel: "web",
        role: "lucina",
        text: `🔧 drive {"action":"list","page_size":50} → ok`,
        meta: { kind: "tool", ok: true, tool: "drive" },
      })),
      { id: "1", ts: 1000, channel: "web", role: "user", text: "Sort my university folder" },
    ];
    stubFetch((c) => (c.url.startsWith("/v1/timeline") ? { body: { items, nextBefore: null, cursor: "5" } } : { body: {} }));
    renderAt("/", () => <Route path="/" component={ChatView} />);
    await waitFor(() => expect(screen.getByText("Worked on Drive · 3 steps")).toBeInTheDocument());
    // The three steps are inside the closed line, not bubbles of their own.
    const steps = screen.getAllByText(/page_size/);
    expect(steps).toHaveLength(3);
    for (const s of steps) expect(s.closest("details")?.open).toBe(false);
    expect(screen.getByText("Done: the folder is sorted.")).toBeInTheDocument();
  });
});
