import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ControlState } from "../api/computer";
import { toScreen } from "../api/computer";
import { setLocale } from "../i18n";
import { stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
import ComputerView, { auditLine, callerName, contentRect, driverLabel, pointIn } from "./Computer";

const VIEW_ID = "v".repeat(64);

function state(over: Partial<ControlState> = {}): ControlState {
  return { sandbox: "workspace", driver: "none", epoch: 0, owner: null, task: null, lastAgent: null, leaseSecs: 45, ...over };
}

/** A fake Lucina: who drives changes with take/release, views carry the mode. */
function fakeLucina(initial: ControlState) {
  let s = initial;
  const calls = stubFetch((c: Call) => {
    if (c.url.startsWith("/v1/computer/control") && c.method === "GET") {
      return { body: { state: s, recent: [], viewer: { assets: "0123456789abcdef" } } };
    }
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
      return {
        body: {
          viewId: VIEW_ID,
          viewUrl: `/v1/computer/view/${VIEW_ID}/viewer/#ticket=t-${mode}&clipboard=off&audio=off&mode=${mode}`,
          sandbox: "workspace",
          mode,
          ttlSecs: 600,
        },
      };
    }
    if (c.url === `/v1/computer/view/${VIEW_ID}/ticket`) {
      const mode = (c.body as { mode: "watch" | "control" }).mode;
      return { body: { ticket: `t-${mode}-renewed`, mode, sandbox: "workspace", ttlSecs: 600, epoch: s.epoch, changed: true } };
    }
    if (c.url === "/v1/computer/input") return { body: { done: (c.body as { events: unknown[] }).events.length, outputs: [] } };
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
  return calls;
}

function renderComputer() {
  return renderAt("/computer", () => <Route path="/computer" component={ComputerView} />);
}

/** The iframe's postMessage, recorded: that is how a ticket reaches the running viewer. */
function watchFrame(frame: HTMLIFrameElement) {
  const posted: unknown[] = [];
  Object.defineProperty(frame, "contentWindow", {
    configurable: true,
    value: { postMessage: (msg: unknown) => posted.push(msg) },
  });
  return posted;
}

/** A pointer event jsdom can dispatch (it has no PointerEvent of its own). */
function pointer(type: string, props: Record<string, unknown> = {}): Event {
  const e = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperty(e, "pointerId", { value: props.pointerId ?? 1, configurable: true });
  Object.defineProperty(e, "pointerType", { value: props.pointerType ?? "touch", configurable: true });
  Object.defineProperty(e, "clientX", { value: props.clientX ?? 0, configurable: true });
  Object.defineProperty(e, "clientY", { value: props.clientY ?? 0, configurable: true });
  return e;
}

/** The screenshot is laid out 640×400 for a 1280×800 desktop, so a client point halves. */
function layoutShot(img: HTMLImageElement) {
  img.getBoundingClientRect = () => ({ left: 0, top: 0, width: 640, height: 400, right: 640, bottom: 400, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;
  Object.defineProperty(img, "naturalWidth", { configurable: true, value: 1280 });
  Object.defineProperty(img, "naturalHeight", { configurable: true, value: 800 });
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

  it("watches live until the owner takes over, then drives with a new ticket on the same view", async () => {
    const calls = fakeLucina(state());
    renderComputer();
    expect(await screen.findByText("Idle")).toBeInTheDocument();
    expect(document.querySelectorAll(".computer-status").length).toBe(1);
    const frame = (await screen.findByTitle("Lucina's screen")) as HTMLIFrameElement;
    const initialSrc = frame.getAttribute("src")!;
    expect(initialSrc).toContain("ticket=t-watch");
    expect(initialSrc).toContain("mode=watch");
    expect(frame.getAttribute("sandbox")).toBe("allow-scripts allow-pointer-lock");
    const posted = watchFrame(frame);

    fireEvent.click(screen.getByRole("button", { name: "Take over" }));
    expect(await screen.findByText("You are driving")).toBeInTheDocument();
    // The ticket is re-issued for the same view and handed to the running viewer: no reload.
    await waitFor(() => expect(posted).toContainEqual({ type: "lucina:ticket", ticket: "t-control-renewed", mode: "control" }));
    expect(calls.some((c) => c.url === `/v1/computer/view/${VIEW_ID}/ticket`)).toBe(true);
    expect(screen.getByTitle("Lucina's screen").getAttribute("src")).toBe(initialSrc);
    expect(calls.filter((c) => c.url === "/v1/computer/view").length).toBe(1);
    expect(calls.find((c) => c.url === "/v1/computer/control" && (c.body as { action: string }).action === "take")?.headers["x-secretary-csrf"]).toBe("1");

    fireEvent.input(screen.getByLabelText("Type on Lucina's computer"), { target: { value: "hello" } });
    fireEvent.click(screen.getByRole("button", { name: "Type" }));
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/computer/input")).toBe(true));
    expect(calls.find((c) => c.url === "/v1/computer/input")?.body).toEqual({ sandbox: "workspace", events: [{ kind: "type", text: "hello" }] });

    await waitFor(() => expect(screen.getByRole("button", { name: "Give back" })).not.toBeDisabled());
    fireEvent.click(screen.getByRole("button", { name: "Give back" }));
    expect(await screen.findByText("Idle")).toBeInTheDocument();
    await waitFor(() => expect(posted).toContainEqual({ type: "lucina:ticket", ticket: "t-watch-renewed", mode: "watch" }));
    expect(screen.getByTitle("Lucina's screen").getAttribute("src")).toBe(initialSrc);
    expect(screen.queryByLabelText("Type on Lucina's computer")).toBeNull();
  });

  it("preloads the immutable viewer as soon as the state says which build it is", async () => {
    fakeLucina(state());
    renderComputer();
    await screen.findByText("Idle");
    const link = document.head.querySelector('link[data-lucina="viewer-preload"]');
    expect(link?.getAttribute("href")).toBe("/v1/computer/viewer-assets/0123456789abcdef/viewer.js");
    expect(link?.getAttribute("rel")).toBe("preload");
  });

  it("asks the viewer for its keyboard only when the Keyboard button is pressed", async () => {
    fakeLucina(state());
    renderComputer();
    await screen.findByText("Idle");
    const frame = (await screen.findByTitle("Lucina's screen")) as HTMLIFrameElement;
    const posted = watchFrame(frame);
    expect(screen.queryByRole("button", { name: "Keyboard" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Take over" }));
    expect(await screen.findByText("You are driving")).toBeInTheDocument();
    expect(posted).not.toContainEqual(expect.objectContaining({ type: "lucina:keyboard" }));
    const keyboard = screen.getByRole("button", { name: "Keyboard" });
    expect(keyboard.getAttribute("aria-pressed")).toBe("false");
    fireEvent.click(keyboard);
    expect(posted).toContainEqual({ type: "lucina:keyboard", open: true });
    expect(keyboard.getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(keyboard);
    expect(posted).toContainEqual({ type: "lucina:keyboard", open: false });
  });

  it("sends the key buttons through the running viewer in Live mode, not as input batches", async () => {
    const calls = fakeLucina(state());
    renderComputer();
    await screen.findByText("Idle");
    const posted = watchFrame((await screen.findByTitle("Lucina's screen")) as HTMLIFrameElement);
    fireEvent.click(screen.getByRole("button", { name: "Take over" }));
    await screen.findByText("You are driving");
    fireEvent.click(screen.getByRole("button", { name: "Esc" }));
    fireEvent.click(screen.getByRole("button", { name: "Ctrl+L" }));
    expect(posted).toContainEqual({ type: "lucina:keys", keys: ["escape"] });
    expect(posted).toContainEqual({ type: "lucina:keys", keys: ["control", "l"] });
    expect(calls.some((c) => c.url === "/v1/computer/input")).toBe(false);
  });

  it("full screen fixes the screen, locks the page and shows the mini bar", async () => {
    fakeLucina(state());
    renderComputer();
    await screen.findByText("Idle");
    const screenEl = document.querySelector(".computer-screen") as HTMLElement;
    expect(screenEl.classList.contains("computer-screen--full")).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Full screen" }));
    await waitFor(() => expect(screenEl.classList.contains("computer-screen--full")).toBe(true));
    expect(document.documentElement.classList.contains("computer-fullscreen")).toBe(true);
    expect(document.querySelector(".computer-page")?.getAttribute("data-full")).toBe("yes");
    const bar = screen.getByRole("toolbar", { name: "Full screen controls" });
    expect(bar).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Exit full screen" })).toHaveLength(2);
    fireEvent.click(screen.getAllByRole("button", { name: "Exit full screen" })[1]!);
    await waitFor(() => expect(screenEl.classList.contains("computer-screen--full")).toBe(false));
    expect(document.documentElement.classList.contains("computer-fullscreen")).toBe(false);
    expect(screen.queryByRole("toolbar", { name: "Full screen controls" })).toBeNull();
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

describe("touch is a mouse in Simple mode", () => {
  beforeEach(() => {
    setLocale("en");
    localStorage.clear();
    localStorage.setItem("computer.mode", "simple");
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  /** Driving, in Simple mode, with the screenshot laid out 640×400. */
  async function driving() {
    const calls = fakeLucina(state({ driver: "none" }));
    renderComputer();
    const img = (await screen.findByAltText("Screenshot of Lucina's screen")) as HTMLImageElement;
    layoutShot(img);
    await screen.findByText("Idle");
    fireEvent.click(screen.getByRole("button", { name: "Take over" }));
    await screen.findByText("You are driving");
    const inputs = () => calls.filter((c) => c.url === "/v1/computer/input").map((c) => c.body as { events: { kind: string }[] });
    return { img, inputs };
  }

  it("a tap is a left click where the finger was", async () => {
    const { img, inputs } = await driving();
    fireEvent(img, pointer("pointerdown", { clientX: 320, clientY: 200 }));
    fireEvent(img, pointer("pointerup", { clientX: 320, clientY: 200 }));
    await waitFor(() => expect(inputs().some((b) => b.events[0]?.kind === "click")).toBe(true));
    expect(inputs().find((b) => b.events[0]?.kind === "click")?.events).toEqual([{ kind: "click", x: 640, y: 400, button: "left" }]);
  });

  it("a long press is a right click, and letting go does not also click left", async () => {
    vi.useFakeTimers();
    const { img, inputs } = await driving();
    fireEvent(img, pointer("pointerdown", { clientX: 100, clientY: 100 }));
    vi.advanceTimersByTime(560);
    await waitFor(() => expect(inputs().some((b) => b.events[0]?.kind === "click")).toBe(true));
    fireEvent(img, pointer("pointerup", { clientX: 100, clientY: 100 }));
    const clicks = inputs().filter((b) => (b.events[0] as { kind: string }).kind === "click");
    expect(clicks.length).toBe(1);
    expect(clicks[0]?.events).toEqual([{ kind: "click", x: 200, y: 200, button: "right" }]);
  });

  it("a drag holds the left button down, moves and releases", async () => {
    const { img, inputs } = await driving();
    fireEvent(img, pointer("pointerdown", { clientX: 100, clientY: 100 }));
    fireEvent(img, pointer("pointermove", { clientX: 200, clientY: 160 }));
    fireEvent(img, pointer("pointerup", { clientX: 200, clientY: 160 }));
    await waitFor(() => expect(inputs().some((b) => b.events[0]?.kind === "up")).toBe(true));
    const events = inputs().flatMap((b) => b.events as { kind: string }[]);
    expect(events.map((e) => e.kind)).toEqual(["down", "move", "up"]);
    expect(events[0]).toEqual({ kind: "down", x: 200, y: 200, button: "left" });
    expect(events[2]).toEqual({ kind: "up", x: 400, y: 320, button: "left" });
  });

  it("two fingers swiping scroll: the direction it went, in wheel steps", async () => {
    const { img, inputs } = await driving();
    fireEvent(img, pointer("pointerdown", { pointerId: 1, clientX: 300, clientY: 200 }));
    fireEvent(img, pointer("pointerdown", { pointerId: 2, clientX: 340, clientY: 240 }));
    fireEvent(img, pointer("pointermove", { pointerId: 1, clientX: 300, clientY: 260 }));
    fireEvent(img, pointer("pointermove", { pointerId: 2, clientX: 340, clientY: 300 }));
    fireEvent(img, pointer("pointerup", { pointerId: 2, clientX: 340, clientY: 300 }));
    await waitFor(() => expect(inputs().some((b) => b.events[0]?.kind === "scroll")).toBe(true));
    const scroll = inputs().find((b) => (b.events[0] as { kind: string }).kind === "scroll");
    expect(scroll?.events[0]).toMatchObject({ kind: "scroll", direction: "down" });
    expect((scroll?.events[0] as unknown as { amount: number }).amount).toBeGreaterThanOrEqual(1);
  });

  it("a two-finger tap is a right click", async () => {
    const { img, inputs } = await driving();
    fireEvent(img, pointer("pointerdown", { pointerId: 1, clientX: 300, clientY: 200 }));
    fireEvent(img, pointer("pointerdown", { pointerId: 2, clientX: 340, clientY: 200 }));
    fireEvent(img, pointer("pointerup", { pointerId: 2, clientX: 340, clientY: 200 }));
    await waitFor(() => expect(inputs().some((b) => b.events[0]?.kind === "click")).toBe(true));
    expect(inputs().find((b) => (b.events[0] as { kind: string }).kind === "click")?.events[0]).toMatchObject({ button: "right" });
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

  it("finds the picture inside a letterboxed box", () => {
    // A 1280×800 desktop in a 400×400 box: 400 wide, 250 high, centred 75px down.
    const shown = contentRect({ left: 10, top: 20, width: 400, height: 400 }, { width: 1280, height: 800 });
    expect(shown).toEqual({ left: 10, top: 95, width: 400, height: 250 });
    expect(pointIn({ clientX: 210, clientY: 220 }, { left: 10, top: 20, width: 400, height: 400 }, { width: 1280, height: 800 })).toEqual({
      x: 640,
      y: 400,
    });
    // A tap in the black bar clamps to the edge of the desktop.
    expect(pointIn({ clientX: 210, clientY: 30 }, { left: 10, top: 20, width: 400, height: 400 }, { width: 1280, height: 800 })).toEqual({
      x: 640,
      y: 0,
    });
  });
});
