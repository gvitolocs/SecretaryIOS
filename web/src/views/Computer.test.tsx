import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ControlState } from "../api/computer";
import { toScreen } from "../api/computer";
import { setLocale } from "../i18n";
import { stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
import { deferred } from "../test/fetch";
import ComputerView, { auditLine, callerName, contentRect, driverLabel, meaningful, passKey, pointIn } from "./Computer";

const VIEW_ID = "v".repeat(64);

function state(over: Partial<ControlState> = {}): ControlState {
  return { sandbox: "workspace", driver: "none", epoch: 0, owner: null, task: null, lastAgent: null, leaseSecs: 45, ...over };
}

/** A fake Lucina: who drives changes with take/release, views carry the mode. */
const LAN = "https://lan.getsecretary.tech:9443";

/**
 * `lan`: the server offers the home-network door; `up`: whether it answers the probe. `desk`: the
 * server mints LucinaDesk tickets (else /v1/computer/desk is a 404, as before it existed).
 */
function fakeLucina(initial: ControlState, lan?: { up: boolean }, desk = false) {
  let s = initial;
  const calls = stubFetch((c: Call) => {
    if (c.url === `${LAN}/v1/computer/lan-probe`) {
      if (!lan?.up) throw new TypeError("Failed to fetch");
      return { status: 204 };
    }
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
          lan: lan ? { url: LAN } : null,
        },
      };
    }
    if (c.url === `/v1/computer/view/${VIEW_ID}/ticket`) {
      const mode = (c.body as { mode: "watch" | "control" }).mode;
      return { body: { ticket: `t-${mode}-renewed`, mode, sandbox: "workspace", ttlSecs: 600, epoch: s.epoch, changed: true } };
    }
    if (c.url === "/v1/computer/desk" && desk) {
      const mode = (c.body as { mode: string }).mode;
      return { body: { ticket: "d".repeat(43), protocol: "lucinadesk.v1", wsPath: "/v1/computer/desk/ws", expiresInSecs: 30, sandbox: "workspace", mode, lan: null } };
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

  it("at home, loads the viewer straight from the home-network door when it answers", async () => {
    const calls = fakeLucina(state(), { up: true });
    renderComputer();
    const frame = (await screen.findByTitle("Lucina's screen")) as HTMLIFrameElement;
    expect(frame.getAttribute("src")).toBe(`${LAN}/v1/computer/view/${VIEW_ID}/viewer/#ticket=t-watch&clipboard=off&audio=off&mode=watch`);
    expect(await screen.findByText("Local network")).toBeInTheDocument();
    const probe = calls.find((c) => c.url === `${LAN}/v1/computer/lan-probe`);
    expect(probe?.credentials).toBe("omit");
  });

  it("falls back to the server silently when the door does not answer", async () => {
    fakeLucina(state(), { up: false });
    renderComputer();
    const frame = (await screen.findByTitle("Lucina's screen")) as HTMLIFrameElement;
    expect(frame.getAttribute("src")?.startsWith(`/v1/computer/view/${VIEW_ID}/viewer/`)).toBe(true);
    expect(await screen.findByText("Remote")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("says Remote when no door is offered (away from home)", async () => {
    const calls = fakeLucina(state());
    renderComputer();
    await screen.findByTitle("Lucina's screen");
    expect(await screen.findByText("Remote")).toBeInTheDocument();
    expect(calls.some((c) => c.url.includes("lan-probe"))).toBe(false);
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

  it("full screen fixes the screen, locks the page and shows the pill", async () => {
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

describe("full screen as a remote desktop (focus mode in Secretary Code)", () => {
  let listener: ((on: boolean) => void) | null = null;
  const setFocus = vi.fn();
  beforeEach(() => {
    setLocale("en");
    localStorage.clear();
    listener = null;
    setFocus.mockReset();
    window.secretaryShell = {
      version: 1,
      showCode: vi.fn(),
      setFocusMode: setFocus,
      onFocusModeChange: (l: (on: boolean) => void) => {
        listener = l;
        return () => (listener = null);
      },
    } as unknown as typeof window.secretaryShell;
  });
  afterEach(() => {
    delete window.secretaryShell;
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  const page = () => document.querySelector(".computer-page")?.getAttribute("data-full");

  /** A message from the viewer frame itself (the page checks the source window). */
  function fromFrame(frame: HTMLIFrameElement, data: unknown) {
    const ev = new MessageEvent("message", { data });
    Object.defineProperty(ev, "source", { value: frame.contentWindow });
    window.dispatchEvent(ev);
  }

  it("asks the shell for focus mode instead of the Fullscreen API, and follows it out", async () => {
    fakeLucina(state());
    const fs = vi.fn(() => Promise.resolve());
    HTMLElement.prototype.requestFullscreen = fs as unknown as typeof HTMLElement.prototype.requestFullscreen;
    renderComputer();
    await screen.findByText("Idle");
    fireEvent.click(screen.getByRole("button", { name: "Full screen" }));
    expect(setFocus).toHaveBeenCalledWith(true);
    expect(fs).not.toHaveBeenCalled();
    expect(page()).toBe("yes");
    expect(screen.getByRole("toolbar", { name: "Full screen controls" })).toBeInTheDocument();
    // The shell leaves focus mode by itself (Esc, its Exit focus button): the page follows.
    listener?.(false);
    await waitFor(() => expect(page()).toBe("no"));
    // And the shell entering it (e.g. after a refused requestFullscreen) turns the layout on.
    listener?.(true);
    await waitFor(() => expect(page()).toBe("yes"));
  });

  it("the viewer's own fullscreen icon does the same as the page's button", async () => {
    fakeLucina(state());
    renderComputer();
    await screen.findByText("Idle");
    const frame = (await screen.findByTitle("Lucina's screen")) as HTMLIFrameElement;
    watchFrame(frame);
    fromFrame(frame, { type: "lucina:fullscreen" });
    expect(setFocus).toHaveBeenCalledWith(true);
    await waitFor(() => expect(page()).toBe("yes"));
    fromFrame(frame, { type: "lucina:fullscreen" });
    expect(setFocus).toHaveBeenLastCalledWith(false);
    await waitFor(() => expect(page()).toBe("no"));
  });

  it("the first click on the stream takes over and is replayed into the desktop", async () => {
    const calls = fakeLucina(state());
    renderComputer();
    await screen.findByText("Idle");
    const frame = (await screen.findByTitle("Lucina's screen")) as HTMLIFrameElement;
    const posted = watchFrame(frame);
    frame.getBoundingClientRect = () => ({ left: 10, top: 20, width: 800, height: 500, right: 810, bottom: 520, x: 10, y: 20, toJSON: () => ({}) }) as DOMRect;
    fireEvent.click(screen.getByRole("button", { name: "Full screen" }));
    const catcher = await waitFor(() => document.querySelector(".computer-catch") as HTMLElement);
    fireEvent.pointerDown(catcher, { clientX: 110, clientY: 220, button: 0 });
    await waitFor(() => expect(posted).toContainEqual({ type: "lucina:pass", click: { x: 100, y: 200, button: "left" } }));
    expect(calls.some((c) => c.url === "/v1/computer/control" && (c.body as { action: string }).action === "take")).toBe(true);
    // The control ticket reaches the viewer before the replayed click.
    const ticketAt = posted.findIndex((m) => (m as { type: string }).type === "lucina:ticket");
    const passAt = posted.findIndex((m) => (m as { type: string }).type === "lucina:pass");
    expect(ticketAt).toBeGreaterThanOrEqual(0);
    expect(ticketAt).toBeLessThan(passAt);
    expect(await screen.findByText("You're driving")).toBeInTheDocument();
    expect(document.querySelector(".computer-catch")).toBeNull();
  });

  it("a key press while watching in full screen takes over and types that key", async () => {
    fakeLucina(state());
    renderComputer();
    await screen.findByText("Idle");
    const posted = watchFrame((await screen.findByTitle("Lucina's screen")) as HTMLIFrameElement);
    fireEvent.click(screen.getByRole("button", { name: "Full screen" }));
    fireEvent.keyDown(window, { key: "a" });
    await waitFor(() => expect(posted).toContainEqual({ type: "lucina:pass", text: "a" }));
  });

  it("asks in a small overlay, not a browser dialog, when Lucina is mid-task", async () => {
    fakeLucina(state({ driver: "lucina", task: { who: "lucina", what: "open example.com", since: 1, stopping: false } }));
    const confirm = vi.spyOn(window, "confirm");
    renderComputer();
    await screen.findByText("Lucina is driving");
    const posted = watchFrame((await screen.findByTitle("Lucina's screen")) as HTMLIFrameElement);
    fireEvent.click(screen.getByRole("button", { name: "Full screen" }));
    const catcher = await waitFor(() => document.querySelector(".computer-catch") as HTMLElement);
    fireEvent.pointerDown(catcher, { clientX: 5, clientY: 5 });
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog.textContent).toContain("open example.com");
    expect(confirm).not.toHaveBeenCalled();
    fireEvent.click(screen.getAllByRole("button", { name: "Take over" }).find((b) => dialog.contains(b))!);
    await waitFor(() => expect(posted.some((m) => (m as { type: string }).type === "lucina:pass")).toBe(true));
  });

  it("Esc leaves focus mode while watching, from the page or from inside the viewer", async () => {
    fakeLucina(state());
    renderComputer();
    await screen.findByText("Idle");
    const frame = (await screen.findByTitle("Lucina's screen")) as HTMLIFrameElement;
    watchFrame(frame);
    fireEvent.click(screen.getByRole("button", { name: "Full screen" }));
    fireEvent.keyDown(window, { key: "Escape" });
    expect(setFocus).toHaveBeenLastCalledWith(false);
    await waitFor(() => expect(page()).toBe("no"));
    fireEvent.click(screen.getByRole("button", { name: "Full screen" }));
    expect(page()).toBe("yes");
    fromFrame(frame, { type: "lucina:escape" });
    expect(setFocus).toHaveBeenLastCalledWith(false);
    await waitFor(() => expect(page()).toBe("no"));
  });

  it("Esc while driving goes to the desktop and keeps focus mode", async () => {
    fakeLucina(state());
    renderComputer();
    await screen.findByText("Idle");
    const frame = (await screen.findByTitle("Lucina's screen")) as HTMLIFrameElement;
    const posted = watchFrame(frame);
    fireEvent.click(screen.getByRole("button", { name: "Full screen" }));
    fireEvent.pointerDown(await waitFor(() => document.querySelector(".computer-catch") as HTMLElement), { clientX: 1, clientY: 1 });
    await screen.findByText("You're driving");
    setFocus.mockClear();
    fireEvent.keyDown(window, { key: "Escape" });
    fromFrame(frame, { type: "lucina:escape" });
    expect(posted).toContainEqual({ type: "lucina:keys", keys: ["escape"] });
    expect(setFocus).not.toHaveBeenCalledWith(false);
    expect(page()).toBe("yes");
  });

  it("leaving focus mode gives back a desktop taken in focus mode", async () => {
    const calls = fakeLucina(state());
    renderComputer();
    await screen.findByText("Idle");
    watchFrame((await screen.findByTitle("Lucina's screen")) as HTMLIFrameElement);
    fireEvent.click(screen.getByRole("button", { name: "Full screen" }));
    fireEvent.pointerDown(await waitFor(() => document.querySelector(".computer-catch") as HTMLElement), { clientX: 1, clientY: 1 });
    await screen.findByText("You're driving");
    listener?.(false);
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/computer/control" && (c.body as { action: string }).action === "release")).toBe(true));
  });
});

describe("holding control", () => {
  beforeEach(() => {
    setLocale("en");
    localStorage.clear();
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("keeps control for a minute with no input at all (heartbeats, polls, no release)", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const calls = fakeLucina(state());
    renderComputer();
    await screen.findByText("Idle");
    fireEvent.click(screen.getByRole("button", { name: "Take over" }));
    await screen.findByText("You are driving");
    await vi.advanceTimersByTimeAsync(60_000);
    const actions = calls.filter((c) => c.url === "/v1/computer/control" && c.method === "POST").map((c) => (c.body as { action: string }).action);
    expect(actions.filter((a) => a === "release")).toEqual([]);
    expect(actions.filter((a) => a === "heartbeat").length).toBeGreaterThanOrEqual(3);
    expect(screen.getByText("You are driving")).toBeInTheDocument();
    expect(calls.some((c) => c.url.endsWith("/ticket") && (c.body as { mode: string }).mode === "watch")).toBe(false);
  });

  it("a state poll that started before the takeover cannot undo it", async () => {
    let s = state();
    const slow = deferred<void>();
    let held = false;
    stubFetch(async (c: Call) => {
      if (c.url.startsWith("/v1/computer/control") && c.method === "GET") {
        const snapshot = s;
        if (held) await slow.promise; // this poll answers late, with the state from before the take
        return { body: { state: snapshot, recent: [] } };
      }
      if (c.url === "/v1/computer/control") {
        const action = (c.body as { action: string }).action;
        if (action === "take") s = state({ driver: "owner", epoch: 1, owner: { device: "web", since: 1, expiresInMs: 45000 } });
        if (action === "release") s = state({ epoch: 2 });
        return { body: { state: s, stoppedTask: null } };
      }
      if (c.url === "/v1/computer/view") {
        const mode = (c.body as { mode: string }).mode;
        return { body: { viewId: VIEW_ID, viewUrl: `/v1/computer/view/${VIEW_ID}/viewer/#ticket=t&mode=${mode}`, sandbox: "workspace", mode, ttlSecs: 600 } };
      }
      if (c.url === `/v1/computer/view/${VIEW_ID}/ticket`) {
        const mode = (c.body as { mode: string }).mode;
        return { body: { ticket: "t2", mode, sandbox: "workspace", ttlSecs: 600, epoch: 1, changed: true } };
      }
      return { status: 404, body: { error: { code: "not_found", message: "no" } } };
    });
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderComputer();
    await screen.findByText("Idle");
    held = true;
    await vi.advanceTimersByTimeAsync(3_100); // a poll starts and hangs
    held = false;
    fireEvent.click(screen.getByRole("button", { name: "Take over" }));
    await screen.findByText("You are driving");
    slow.resolve(); // the stale "nobody drives" answer arrives now
    await vi.advanceTimersByTimeAsync(100);
    expect(screen.getByText("You are driving")).toBeInTheDocument();
    expect(screen.queryByText("Your takeover ended.")).toBeNull();
  });
});

describe("page helpers", () => {
  it("passes keys the way the viewer names them", () => {
    expect(passKey({ key: "a" })).toEqual({ text: "a" });
    expect(passKey({ key: "Enter" })).toEqual({ keys: ["enter"] });
    expect(passKey({ key: "ArrowLeft" })).toEqual({ keys: ["arrowleft"] });
    expect(passKey({ key: "l", ctrlKey: true })).toEqual({ keys: ["control", "l"] });
    expect(passKey({ key: "Shift" })).toBeNull();
  });

  it("keeps sockets, tickets and live byte counts out of Recent activity", () => {
    expect(meaningful({ at: 1, event: "take", device: "web" } as never)).toBe(true);
    expect(meaningful({ at: 1, event: "view_socket_closed", why: "sandbox closed" } as never)).toBe(false);
    expect(meaningful({ at: 1, event: "view_ticket" } as never)).toBe(false);
    expect(meaningful({ at: 1, event: "input", batch: { via: "live", bytes: 1210 } } as never)).toBe(false);
    expect(meaningful({ at: 1, event: "input", batch: { kinds: ["click"] } } as never)).toBe(true);
  });
});

describe("ComputerView with LucinaDesk", () => {
  const sockets: string[][] = [];
  beforeEach(() => {
    sockets.length = 0;
    // A browser with WebCodecs: LucinaDesk is the Live view.
    vi.stubGlobal("VideoDecoder", Object.assign(function () {}, { isConfigSupported: async () => ({ supported: true }) }));
    vi.stubGlobal("EncodedVideoChunk", function () {});
    vi.stubGlobal(
      "WebSocket",
      class {
        static OPEN = 1;
        readyState = 0;
        constructor(_url: string, protocols: string[]) {
          sockets.push(protocols);
        }
        close() {}
        send() {}
      },
    );
  });

  it("shows LucinaDesk and mints no Cua view; the page's takeover bar steps aside", async () => {
    const calls = fakeLucina(state(), undefined, true);
    renderComputer();
    expect(await screen.findByRole("img", { name: "Lucina's screen" })).toBeInTheDocument();
    await waitFor(() => expect(sockets).toHaveLength(1));
    expect(sockets[0]![0]).toBe("lucinadesk.v1");
    expect(calls.some((c) => c.url === "/v1/computer/desk")).toBe(true);
    expect(calls.some((c) => c.url === "/v1/computer/view")).toBe(false);
    expect(screen.queryByTitle("Lucina's screen")).toBeNull();
    expect(screen.queryByRole("button", { name: "Take over" })).toBeNull();
  });

  it("the player's own full screen does not switch on the page's classic full-screen layout", async () => {
    fakeLucina(state(), undefined, true);
    renderComputer();
    const canvas = await screen.findByRole("img", { name: "Lucina's screen" });
    Object.defineProperty(document, "fullscreenElement", { configurable: true, get: () => canvas.closest(".ldesk-stage") });
    try {
      document.dispatchEvent(new Event("fullscreenchange"));
      expect(document.querySelector(".computer-page")!.getAttribute("data-full")).toBe("no");
    } finally {
      delete (document as { fullscreenElement?: unknown }).fullscreenElement;
    }
  });

  it("falls back to the Cua viewer when the server has no desk endpoint", async () => {
    const calls = fakeLucina(state());
    renderComputer();
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/computer/view")).toBe(true));
    await waitFor(() => expect(document.querySelector("iframe")).not.toBeNull());
    expect(screen.queryByRole("img", { name: "Lucina's screen" })).toBeNull();
  });

  it("remembers the classic viewer, and offers LucinaDesk back", async () => {
    localStorage.setItem("computer.classic", "yes");
    const calls = fakeLucina(state(), undefined, true);
    renderComputer();
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/computer/view")).toBe(true));
    fireEvent.click(screen.getByRole("button", { name: "Use LucinaDesk" }));
    expect(await screen.findByRole("img", { name: "Lucina's screen" })).toBeInTheDocument();
    expect(localStorage.getItem("computer.classic")).toBe("no");
  });
});
