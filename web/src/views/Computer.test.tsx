import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ControlState } from "../api/computer";
import { toScreen } from "../api/computer";
import { setLocale } from "../i18n";
import { stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
import ComputerView, { auditLine, callerName, driverLabel } from "./Computer";

function state(over: Partial<ControlState> = {}): ControlState {
  return { sandbox: "workspace", driver: "none", epoch: 0, owner: null, task: null, lastAgent: null, leaseSecs: 45, ...over };
}

/** A fake Lucina: who drives changes with take/release, views carry the mode. */
function fakeLucina(initial: ControlState) {
  let s = initial;
  const calls = stubFetch((c: Call) => {
    if (c.url.startsWith("/v1/computer/control") && c.method === "GET") return { body: { state: s, recent: [] } };
    if (c.url === "/v1/computer/control") {
      const action = (c.body as { action: string }).action;
      if (action === "take") {
        const stopped = s.task ? `${s.task.who}: ${s.task.what}` : null;
        s = state({ driver: "owner", epoch: s.epoch + 1, owner: { device: "web", since: 1, expiresInMs: 45000 } });
        return { body: { state: s, stoppedTask: stopped } };
      }
      if (action === "release") {
        s = state({ epoch: s.epoch + 1 });
        return { body: { state: s } };
      }
      return { body: { state: s } };
    }
    if (c.url === "/v1/computer/view") {
      const mode = (c.body as { mode: string }).mode;
      return { body: { viewId: "v".repeat(64), viewUrl: `/v1/computer/view/${"v".repeat(64)}/viewer/#ticket=t-${mode}`, sandbox: "workspace", mode, ttlSecs: 600 } };
    }
    if (c.url === "/v1/computer/input") return { body: { done: (c.body as { events: unknown[] }).events.length, outputs: [] } };
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
  return calls;
}

function renderComputer() {
  return renderAt("/computer", () => <Route path="/computer" component={ComputerView} />);
}

describe("Lucina's computer", () => {
  beforeEach(() => {
    setLocale("en");
    localStorage.clear();
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("watches live until the owner takes over, then drives with a control view, then gives it back", async () => {
    const calls = fakeLucina(state());
    renderComputer();
    expect(await screen.findByText("Idle")).toBeInTheDocument();
    expect(document.querySelectorAll(".computer-status").length).toBe(1);
    const frame = await screen.findByTitle("Lucina's screen");
    expect(frame.getAttribute("src")).toContain("ticket=t-watch");
    expect(frame.getAttribute("sandbox")).toBe("allow-scripts allow-pointer-lock");

    fireEvent.click(screen.getByRole("button", { name: "Take over" }));
    expect(await screen.findByText("You are driving")).toBeInTheDocument();
    expect(document.querySelectorAll(".computer-status").length).toBe(1);
    await waitFor(() => expect(screen.getByTitle("Lucina's screen").getAttribute("src")).toContain("ticket=t-control"));
    expect(calls.find((c) => c.url === "/v1/computer/control" && (c.body as { action: string }).action === "take")?.headers["x-secretary-csrf"]).toBe("1");

    fireEvent.input(screen.getByLabelText("Type on Lucina's computer"), { target: { value: "hello" } });
    fireEvent.click(screen.getByRole("button", { name: "Type" }));
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/computer/input")).toBe(true));
    expect(calls.find((c) => c.url === "/v1/computer/input")?.body).toEqual({ sandbox: "workspace", events: [{ kind: "type", text: "hello" }] });

    await waitFor(() => expect(screen.getByRole("button", { name: "Give back" })).not.toBeDisabled());
    fireEvent.click(screen.getByRole("button", { name: "Give back" }));
    expect(await screen.findByText("Idle")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByTitle("Lucina's screen").getAttribute("src")).toContain("ticket=t-watch"));
    expect(screen.queryByLabelText("Type on Lucina's computer")).toBeNull();
  });

  it("asks before stopping Lucina's task and says what it stopped", async () => {
    fakeLucina(state({ driver: "lucina", task: { who: "lucina", what: "open example.com", since: 1, stopping: false } }));
    const confirm = vi.spyOn(window, "confirm").mockReturnValueOnce(false).mockReturnValueOnce(true);
    renderComputer();
    expect(await screen.findByText("Lucina is driving")).toBeInTheDocument();
    expect(screen.getByText("Working on: open example.com")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Take over" }));
    expect(confirm).toHaveBeenCalledWith(expect.stringContaining("open example.com"));
    expect(screen.queryByText("You are driving")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Take over" }));
    expect(await screen.findByText("Stopped: lucina: open example.com")).toBeInTheDocument();
  });

  it("simple mode clicks on the screenshot only while driving", async () => {
    localStorage.setItem("computer.mode", "simple");
    const calls = fakeLucina(state());
    renderComputer();
    const img = (await screen.findByAltText("Screenshot of Lucina's screen")) as HTMLImageElement;
    expect(img.getAttribute("src")).toMatch(/^\/v1\/computer\/screenshot\?sandbox=workspace&t=\d+$/);
    fireEvent.click(img);
    expect(calls.some((c) => c.url === "/v1/computer/input")).toBe(false);
    await screen.findByText("Idle");
    fireEvent.click(screen.getByRole("button", { name: "Take over" }));
    await screen.findByText("You are driving");
    fireEvent.click(screen.getByRole("button", { name: "↵" }));
    await waitFor(() =>
      expect(calls.find((c) => c.url === "/v1/computer/input")?.body).toEqual({ sandbox: "workspace", events: [{ kind: "key", key: "Return" }] }),
    );
    expect(calls.some((c) => c.url === "/v1/computer/view")).toBe(false);
  });
});

describe("helpers", () => {
  beforeEach(() => setLocale("en"));

  it("names drivers", () => {
    expect(callerName("mcp:claude")).toBe("Claude");
    expect(callerName("mcp:grok-bot")).toBe("Grok bot");
    expect(driverLabel(state({ driver: "agent", lastAgent: { who: "mcp:claude", at: 1 } }), false)).toBe("Claude is driving");
    expect(driverLabel(state({ driver: "lucina", task: { who: "mcp:grok", what: "x", since: 1, stopping: false } }), false)).toBe("Grok is driving");
    expect(driverLabel(state({ driver: "owner", owner: { device: "iPhone", since: 1, expiresInMs: 1 } }), false)).toBe("Driving from iPhone");
  });

  it("describes audit entries without typed text", () => {
    expect(auditLine({ at: 1, sandbox: "workspace", event: "input", device: "iPhone", batch: { via: "fallback", kinds: ["type", "key"], textChars: 5 } })).toBe(
      "iPhone: input type, key",
    );
    expect(auditLine({ at: 1, sandbox: "workspace", event: "input", device: "Mac", batch: { via: "live", bytes: 120 } })).toBe("Mac: input 120 B");
    expect(auditLine({ at: 1, sandbox: "workspace", event: "take", device: "Mac" })).toBe("Mac took over");
  });

  it("maps a click on the scaled screenshot to sandbox pixels", () => {
    expect(toScreen({ offsetX: 320, offsetY: 200 }, { width: 640, height: 400 }, { width: 1280, height: 800 })).toEqual({ x: 640, y: 400 });
    expect(toScreen({ offsetX: 9999, offsetY: -5 }, { width: 640, height: 400 }, { width: 1280, height: 800 })).toEqual({ x: 1279, y: 0 });
  });
});
