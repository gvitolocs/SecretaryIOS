import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ControlState } from "../api/computer";
import { toScreen } from "../api/computer";
import { setLocale } from "../i18n";
import { stubWide } from "../test/wide";
import { stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
import { deferred } from "../test/fetch";
import ComputerView, { auditLine, callerName, contentRect, driverLabel, meaningful, passKey, pointIn, relayClipboard } from "./Computer";

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

  it("uses the desktop page header, sections and viewer card without the phone toolbar", async () => {
    stubWide();
    fakeLucina(state());
    renderComputer();
    expect(screen.getByRole("heading", { level: 1, name: "Lucina's computer" })).toBeInTheDocument();
    const nav = screen.getByRole("navigation", { name: "Which computer" });
    expect(within(nav).getByRole("link", { name: "Desktop" })).toHaveAttribute("aria-current", "page");
    fireEvent.click(within(nav).getByRole("link", { name: "Android" }));
    expect(within(nav).getByRole("link", { name: "Android" })).toHaveAttribute("aria-current", "page");
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(1);
    expect(document.querySelector(".computer-status, .computer-seg, #computer-title")).toBeNull();
    expect(screen.getByRole("region", { name: "Lucina's screen" })).toHaveClass("pg-card");
    expect(await screen.findByTitle("Lucina's screen")).toBeInTheDocument();
  });

  it("keeps the desktop header visible when the computer cannot load", async () => {
    stubWide();
    stubFetch(() => ({ status: 404, body: { error: { code: "not_found", message: "Not found." } } }));
    renderComputer();
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Lucina's computer");
    expect(await screen.findByRole("alert")).toHaveClass("pg-error");
    expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();

  it("shows the step Lucina's engine is on while she works", async () => {
    fakeLucina(
      state({
        driver: "lucina",
        task: { who: "lucina", what: "find flights to London", since: 1, stopping: false, progress: { line: "step 4 · clicked button \"Search\" · 1.2 s", at: 2 } },
      }),
    );
    renderComputer();
    expect(await screen.findByText("step 4 · clicked button \"Search\" · 1.2 s")).toBeInTheDocument();
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
    expect(auditLine({ at: 1, sandbox: "workspace", event: "jev_step", what: "step 2 · clicked list item \"Billund BLL\" · 1.3 s" })).toBe(
      "step 2 · clicked list item \"Billund BLL\" · 1.3 s",
    );
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

  it("in focus mode the drive bar is the only toolbar: Cua's hides, its buttons are mirrored", async () => {
    fakeLucina(state());
    renderComputer();
    await screen.findByText("Idle");
    const frame = (await screen.findByTitle("Lucina's screen")) as HTMLIFrameElement;
    const posted = watchFrame(frame);
    fromFrame(frame, {
      type: "lucina:buttons",
      buttons: [
        { id: "clipboard", label: "Clipboard sync on", pressed: true },
        { id: "upload", label: "Upload files" },
        { id: "stats", label: "Stats", hidden: true },
        { id: "audio", label: "Sound on", pressed: false },
      ],
    });
    fireEvent.click(screen.getByRole("button", { name: "Full screen" }));
    await waitFor(() => expect(posted).toContainEqual({ type: "lucina:chrome", toolbar: false }));
    const bar = () => within(screen.getByRole("toolbar", { name: "Full screen controls" }));
    fireEvent.click(bar().getByRole("button", { name: "Take over" }));
    await screen.findByText("You're driving");
    const sync = bar().getByRole("button", { name: "Clipboard sync on" });
    expect(sync).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "Sound on" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Upload files" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Stats" })).toBeNull();
    fireEvent.click(sync);
    expect(posted).toContainEqual({ type: "lucina:button", id: "clipboard" });
    // Leaving focus mode brings Cua's toolbar back.
    fireEvent.click(bar().getByRole("button", { name: "Exit full screen" }));
    listener?.(false);
    await waitFor(() => expect(posted).toContainEqual({ type: "lucina:chrome", toolbar: true }));
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

describe("the viewer reconnects by itself", () => {
  beforeEach(() => setLocale("en"));
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  function fromFrame(frame: HTMLIFrameElement, data: unknown) {
    const ev = new MessageEvent("message", { data });
    Object.defineProperty(ev, "source", { value: frame.contentWindow });
    window.dispatchEvent(ev);
  }
  const views = (calls: Call[]) => calls.filter((c) => c.url === "/v1/computer/view").length;

  it("opens a fresh view after a failed stream, backing off, until it streams again", async () => {
    const calls = fakeLucina(state());
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderComputer();
    await waitFor(() => expect(document.querySelector("iframe")).not.toBeNull());
    const frame = document.querySelector("iframe")!;
    watchFrame(frame);
    expect(views(calls)).toBe(1);

    fromFrame(frame, { type: "lucina:state", state: "failed" });
    expect(await screen.findByText("Reconnecting to Lucina's computer…")).toBeInTheDocument();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(views(calls)).toBe(2);
    fromFrame(frame, { type: "lucina:state", state: "failed" });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(views(calls)).toBe(2); // the second wait is 2 s
    await vi.advanceTimersByTimeAsync(1_000);
    expect(views(calls)).toBe(3);

    fromFrame(frame, { type: "lucina:state", state: "streaming" });
    await waitFor(() => expect(screen.queryByText("Reconnecting to Lucina's computer…")).toBeNull());
    fromFrame(frame, { type: "lucina:state", state: "failed" });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(views(calls)).toBe(4); // streaming reset the backoff
  });

  it("treats a frame that never says ready (a raw error answer) as down", async () => {
    const calls = fakeLucina(state());
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderComputer();
    await waitFor(() => expect(document.querySelector("iframe")).not.toBeNull());
    const frame = document.querySelector("iframe")!;
    watchFrame(frame);
    frame.dispatchEvent(new Event("load"));
    fromFrame(frame, { type: "lucina:ready" });
    await vi.advanceTimersByTimeAsync(8_000);
    expect(views(calls)).toBe(1);

    frame.dispatchEvent(new Event("load")); // e.g. {"error":"sandbox_unreachable"}
    await vi.advanceTimersByTimeAsync(6_000);
    expect(screen.getByText("Reconnecting to Lucina's computer…")).toBeInTheDocument();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(views(calls)).toBe(2);
  });

  it("watches again when the takeover died with Lucina (a restart drops the lease)", async () => {
    let s = state();
    let restarted = false;
    const calls = stubFetch((c: Call) => {
      if (c.url.startsWith("/v1/computer/control") && c.method === "GET") return { body: { state: s, recent: [] } };
      if (c.url === "/v1/computer/control") {
        s = state({ driver: "owner", epoch: 1, owner: { device: "web", since: 1, expiresInMs: 45000 } });
        return { body: { state: s, stoppedTask: null } };
      }
      if (c.url === `/v1/computer/view/${VIEW_ID}/ticket`) {
        return { body: { ticket: "t2", mode: "control", sandbox: "workspace", ttlSecs: 600, epoch: 1, changed: true } };
      }
      if (c.url === "/v1/computer/view") {
        const mode = (c.body as { mode: string }).mode;
        if (restarted && mode === "control") return { status: 409, body: { error: { code: "not_driving", message: "take over before driving" } } };
        return { body: { viewId: VIEW_ID, viewUrl: `/v1/computer/view/${VIEW_ID}/viewer/#ticket=t&mode=${mode}`, sandbox: "workspace", mode, ttlSecs: 600 } };
      }
      return { status: 404, body: { error: { code: "not_found", message: "no" } } };
    });
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderComputer();
    await waitFor(() => expect(document.querySelector("iframe")).not.toBeNull());
    fireEvent.click(await screen.findByRole("button", { name: "Take over" }));
    await screen.findByText("You are driving");
    const frame = document.querySelector("iframe")!;
    watchFrame(frame);
    restarted = true; // the poll still says "owner": only the 409 tells
    fromFrame(frame, { type: "lucina:state", state: "failed" });
    await vi.advanceTimersByTimeAsync(1_000);
    await waitFor(() => expect(calls.filter((c) => c.url === "/v1/computer/view").map((c) => (c.body as { mode: string }).mode)).toEqual(["watch", "control", "watch"]));
    expect(screen.queryByText("take over before driving")).toBeNull();
    // The fresh view is a fresh iframe: it is the one that reports streaming.
    await waitFor(() => expect(document.querySelector("iframe")?.getAttribute("src")).toContain("mode=watch"));
    const fresh = document.querySelector("iframe")!;
    watchFrame(fresh);
    fromFrame(fresh, { type: "lucina:state", state: "streaming" });
    await waitFor(() => expect(screen.queryByText("Reconnecting to Lucina's computer…")).toBeNull());
    expect(screen.queryByText("You are driving")).toBeNull();
  });

  it("keeps trying while Lucina is away, then shows the viewer", async () => {
    let down = 3;
    const calls = stubFetch((c: Call) => {
      if (c.url.startsWith("/v1/computer/control") && c.method === "GET") {
        return { body: { state: state(), recent: [], viewer: { assets: "0123456789abcdef" } } };
      }
      if (c.url === "/v1/computer/view") {
        if (down-- > 0) return { status: 502, body: { error: { code: "sandbox_unreachable", message: "unreachable" } } };
        return { body: { viewId: VIEW_ID, viewUrl: `/v1/computer/view/${VIEW_ID}/viewer/#ticket=t&mode=watch`, sandbox: "workspace", mode: "watch", ttlSecs: 600 } };
      }
      return { status: 404, body: { error: { code: "not_found", message: "no" } } };
    });
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderComputer();
    expect(await screen.findByText("Reconnecting to Lucina's computer…")).toBeInTheDocument();
    expect(screen.queryByText("unreachable")).toBeNull();
    await vi.advanceTimersByTimeAsync(1_000 + 2_000 + 4_000);
    await waitFor(() => expect(document.querySelector("iframe")).not.toBeNull());
    expect(views(calls)).toBe(4);
  });
});

describe("files while driving", () => {
  beforeEach(() => setLocale("en"));
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  function fromFrame(frame: HTMLIFrameElement, data: unknown) {
    const ev = new MessageEvent("message", { data });
    Object.defineProperty(ev, "source", { value: frame.contentWindow });
    window.dispatchEvent(ev);
  }

  /** A Lucina that lets this page drive, takes files, and has `copied` on her clipboard. */
  function filesLucina(copied: { generation: number; files: { path: string; name: string; bytes: number }[] }) {
    let s = state();
    return stubFetch((c: Call) => {
      if (c.url.startsWith("/v1/computer/control") && c.method === "GET") return { body: { state: s, recent: [] } };
      if (c.url === "/v1/computer/control") {
        if ((c.body as { action: string }).action === "take") {
          s = state({ driver: "owner", epoch: 1, owner: { device: "web", since: 1, expiresInMs: 45000 } });
        }
        return { body: { state: s, stoppedTask: null } };
      }
      if (c.url === "/v1/computer/view") {
        const mode = (c.body as { mode: string }).mode;
        return { body: { viewId: VIEW_ID, viewUrl: `/v1/computer/view/${VIEW_ID}/viewer/#ticket=t&mode=${mode}`, sandbox: "workspace", mode, ttlSecs: 600 } };
      }
      if (c.url === `/v1/computer/view/${VIEW_ID}/ticket`) {
        return { body: { ticket: "t2", mode: "control", sandbox: "workspace", ttlSecs: 600, epoch: 1, changed: true } };
      }
      if (c.url.startsWith("/v1/computer/files?") && c.method === "POST") {
        const name = new URL(c.url, "http://x").searchParams.get("name");
        return { body: { path: `/home/cua/Desktop/From Mac/${name}`, name, bytes: 2 } };
      }
      if (c.url === "/v1/computer/clipboard/files") return { body: { generation: 9 } };
      if (c.url.startsWith("/v1/computer/clipboard/files?")) return { body: copied };
      return { status: 404, body: { error: { code: "not_found", message: "no" } } };
    });
  }

  async function drive() {
    renderComputer();
    await waitFor(() => expect(document.querySelector("iframe")).not.toBeNull());
    fireEvent.click(await screen.findByRole("button", { name: "Take over" }));
    await screen.findByText("You are driving");
    const frame = document.querySelector("iframe")!;
    return { frame, posted: watchFrame(frame) };
  }

  it("sends pasted files to her desktop and clipboard, then lets the frame paste", async () => {
    const calls = filesLucina({ generation: 1, files: [] });
    const { frame, posted } = await drive();
    fromFrame(frame, { type: "lucina:files", id: 7, paste: true, files: [new File(["hi"], "report.pdf"), new File(["yo"], "notes.txt")] });
    await waitFor(() => expect(posted).toContainEqual({ type: "lucina:files-done", id: 7, ok: true }));
    const uploads = calls.filter((c) => c.url.startsWith("/v1/computer/files?"));
    expect(uploads.map((c) => c.url)).toEqual([
      "/v1/computer/files?sandbox=workspace&name=report.pdf",
      "/v1/computer/files?sandbox=workspace&name=notes.txt",
    ]);
    expect(uploads[0]!.headers["x-secretary-csrf"]).toBe("1");
    const set = calls.find((c) => c.url === "/v1/computer/clipboard/files");
    expect(set?.body).toEqual({ sandbox: "workspace", paths: ["/home/cua/Desktop/From Mac/report.pdf", "/home/cua/Desktop/From Mac/notes.txt"] });
    expect(await screen.findByText("2 files are in From Mac on her desktop.")).toBeInTheDocument();
  });

  it("refuses files while only watching", async () => {
    const calls = filesLucina({ generation: 1, files: [] });
    renderComputer();
    await waitFor(() => expect(document.querySelector("iframe")).not.toBeNull());
    const frame = document.querySelector("iframe")!;
    const posted = watchFrame(frame);
    fromFrame(frame, { type: "lucina:files", id: 1, paste: true, files: [new File(["hi"], "a.txt")] });
    await waitFor(() => expect(posted).toContainEqual({ type: "lucina:files-done", id: 1, ok: false }));
    expect(calls.some((c) => c.url.startsWith("/v1/computer/files"))).toBe(false);
  });

  it("offers files copied on her desktop as downloads", async () => {
    const copied = { generation: 1, files: [] as { path: string; name: string; bytes: number }[] };
    filesLucina(copied);
    const { frame } = await drive();
    fromFrame(frame, { type: "lucina:copied" }); // the first look only learns what is there
    await new Promise((r) => setTimeout(r, 50));
    expect(screen.queryByText(/Lucina copied/)).toBeNull();
    copied.generation = 2;
    copied.files = [{ path: "/home/cua/Documents/report.pdf", name: "report.pdf", bytes: 2_202_009 }];
    fromFrame(frame, { type: "lucina:copied" });
    expect(await screen.findByText("Lucina copied report.pdf (2.1 MB).")).toBeInTheDocument();
    const clicked: { href: string; download: string }[] = [];
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
      clicked.push({ href: this.getAttribute("href") ?? "", download: this.download });
    });
    fireEvent.click(screen.getByRole("button", { name: "Download" }));
    expect(clicked).toEqual([
      { href: "/v1/computer/files?sandbox=workspace&path=%2Fhome%2Fcua%2FDocuments%2Freport.pdf", download: "report.pdf" },
    ]);
    expect(screen.queryByText(/Lucina copied/)).toBeNull();
  });
});

describe("giving the desktop back", () => {
  let listener: ((on: boolean) => void) | null = null;
  const setFocus = vi.fn((on: boolean) => setTimeout(() => listener?.(on), 0));
  beforeEach(() => {
    setLocale("en");
    localStorage.clear();
    listener = null;
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
    Object.defineProperty(document, "hidden", { configurable: true, value: false });
  });

  function fromFrame(frame: HTMLIFrameElement, data: unknown) {
    const ev = new MessageEvent("message", { data });
    Object.defineProperty(ev, "source", { value: frame.contentWindow });
    window.dispatchEvent(ev);
  }

  /** A Lucina that lets this page drive; `beat` answers heartbeats (an error code, or fine). */
  function lucina(beat: () => string | null = () => null) {
    let s = state();
    return stubFetch((c: Call) => {
      if (c.url.startsWith("/v1/computer/control") && c.method === "GET") return { body: { state: s, recent: [] } };
      if (c.url === "/v1/computer/control") {
        const action = (c.body as { action: string }).action;
        if (action === "take") s = state({ driver: "owner", epoch: 1, owner: { device: "web", since: 1, expiresInMs: 45000 } });
        if (action === "release") s = state({ epoch: 2 });
        if (action === "heartbeat") {
          const code = beat();
          if (code) {
            s = state({ epoch: 2 });
            return { status: 409, body: { error: { code, message: code } } };
          }
        }
        return { body: { state: s, stoppedTask: null } };
      }
      if (c.url === "/v1/computer/view") {
        const mode = (c.body as { mode: string }).mode;
        return { body: { viewId: VIEW_ID, viewUrl: `/v1/computer/view/${VIEW_ID}/viewer/#ticket=t&mode=${mode}`, sandbox: "workspace", mode, ttlSecs: 600 } };
      }
      if (c.url === `/v1/computer/view/${VIEW_ID}/ticket`) {
        const mode = (c.body as { mode: string }).mode;
        return { body: { ticket: `t-${mode}`, mode, sandbox: "workspace", ttlSecs: 600, epoch: 1, changed: true } };
      }
      return { status: 404, body: { error: { code: "not_found", message: "no" } } };
    });
  }
  const released = (calls: Call[]) => calls.some((c) => c.url === "/v1/computer/control" && (c.body as { action: string }).action === "release");
  const beats = (calls: Call[]) =>
    calls.filter((c) => c.url === "/v1/computer/control" && (c.body as { action: string }).action === "heartbeat").map((c) => (c.body as { active: boolean }).active);

  async function drive() {
    renderComputer();
    await screen.findByText("Idle");
    fireEvent.click(screen.getAllByRole("button", { name: "Take over" })[0]!);
    await screen.findByText("You are driving");
    const frame = document.querySelector("iframe")!;
    watchFrame(frame);
    return frame;
  }

  it("leaving focus mode gives control back, even when it was taken before", async () => {
    const calls = lucina();
    await drive();
    fireEvent.click(screen.getByRole("button", { name: "Full screen" }));
    await waitFor(() => expect(document.querySelector(".computer-page")?.getAttribute("data-full")).toBe("yes"));
    listener?.(false); // Esc or the shell's Exit focus
    await waitFor(() => expect(released(calls)).toBe(true));
    await waitFor(() => expect(screen.queryByText("You are driving")).toBeNull());
  });

  it("heartbeats say whether there was real input; the stream's own counts, clipboard sync does not", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const calls = lucina();
    const frame = await drive();
    await vi.advanceTimersByTimeAsync(15_000);
    expect(beats(calls).at(-1)).toBe(true); // the takeover itself
    await vi.advanceTimersByTimeAsync(15_000);
    expect(beats(calls).at(-1)).toBe(false);
    fromFrame(frame, { type: "lucina:clipboard", id: 1, text: "x" });
    fromFrame(frame, { type: "lucina:copied" });
    await vi.advanceTimersByTimeAsync(15_000);
    expect(beats(calls).at(-1)).toBe(false);
    fromFrame(frame, { type: "lucina:active" });
    await vi.advanceTimersByTimeAsync(15_000);
    expect(beats(calls).at(-1)).toBe(true);
    fireEvent.keyDown(window, { key: "a" });
    await vi.advanceTimersByTimeAsync(15_000);
    expect(beats(calls).at(-1)).toBe(true);
  });

  it("two minutes without input give control back, with one click to take it again", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const calls = lucina();
    await drive();
    await vi.advanceTimersByTimeAsync(119_000);
    expect(released(calls)).toBe(false);
    await vi.advanceTimersByTimeAsync(16_000);
    await waitFor(() => expect(released(calls)).toBe(true));
    const toast = await screen.findByText("You were idle, so Lucina has the computer again.");
    fireEvent.click(within(toast.closest(".computer-idle") as HTMLElement).getByRole("button", { name: "Take over" }));
    await screen.findByText("You are driving");
    expect(screen.queryByText("You were idle, so Lucina has the computer again.")).toBeNull();
  });

  it("Lucina saying the owner was idle shows the same toast", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    lucina(() => "owner_idle");
    await drive();
    await vi.advanceTimersByTimeAsync(15_000);
    expect(await screen.findByText("You were idle, so Lucina has the computer again.")).toBeInTheDocument();
    expect(screen.queryByText("You are driving")).toBeNull();
  });

  it("a page hidden for a minute gives control back, also when it wakes from sleep", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const calls = lucina();
    await drive();
    Object.defineProperty(document, "hidden", { configurable: true, value: true });
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.advanceTimersByTimeAsync(30_000);
    Object.defineProperty(document, "hidden", { configurable: true, value: false });
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.advanceTimersByTimeAsync(60_000);
    expect(released(calls)).toBe(false); // back within the minute: still driving
    Object.defineProperty(document, "hidden", { configurable: true, value: true });
    document.dispatchEvent(new Event("visibilitychange"));
    // Asleep: no timer runs, but the clock moves on.
    vi.setSystemTime(Date.now() + 5 * 60_000);
    Object.defineProperty(document, "hidden", { configurable: true, value: false });
    document.dispatchEvent(new Event("visibilitychange"));
    await waitFor(() => expect(released(calls)).toBe(true));
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

  it("writes what was copied on her desktop to this computer only while driving", async () => {
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    try {
      const replies: boolean[] = [];
      await relayClipboard({ id: 1, text: "copied there" }, true, (ok) => replies.push(ok));
      expect(writeText).toHaveBeenCalledWith("copied there");
      await relayClipboard({ id: 2, text: "while watching" }, false, (ok) => replies.push(ok));
      await relayClipboard({ id: 3, text: 42 }, true, (ok) => replies.push(ok));
      await relayClipboard({ id: 4, text: "x".repeat(1024 * 1024 + 1) }, true, (ok) => replies.push(ok));
      expect(writeText).toHaveBeenCalledTimes(1);
      writeText.mockImplementationOnce(() => Promise.reject(new Error("Document is not focused.")));
      await relayClipboard({ id: 5, text: "unfocused" }, true, (ok) => replies.push(ok));
      expect(replies).toEqual([true, false, false, false, false]);
    } finally {
      delete (navigator as { clipboard?: unknown }).clipboard;
    }
  });

  it("writes an image copied on her desktop as a PNG on this computer", async () => {
    const write = vi.fn((_items: unknown[]) => Promise.resolve());
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { write, writeText: vi.fn() } });
    class Item {
      constructor(public parts: Record<string, Blob>) {}
    }
    vi.stubGlobal("ClipboardItem", Item);
    try {
      const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47]).buffer;
      const replies: boolean[] = [];
      await relayClipboard({ id: 1, png, text: "alt" }, true, (ok) => replies.push(ok));
      expect(replies).toEqual([true]);
      const parts = (write.mock.calls[0]![0] as Item[])[0]!.parts;
      expect(Object.keys(parts).sort()).toEqual(["image/png", "text/plain"]);
      expect(parts["image/png"]!.type).toBe("image/png");
      await relayClipboard({ id: 2, png }, false, (ok) => replies.push(ok));
      expect(write).toHaveBeenCalledTimes(1);
    } finally {
      delete (navigator as { clipboard?: unknown }).clipboard;
      vi.unstubAllGlobals();
    }
  });

  it("shows clipboard syncs as byte counts", () => {
    expect(auditLine({ at: 1, event: "clipboard_to_desktop", bytes: 12 } as never)).toBe("Clipboard → her desktop (12 B)");
    expect(auditLine({ at: 1, event: "clipboard_from_desktop", bytes: 3, imageBytes: 348_160 } as never)).toBe("Clipboard → this computer (3 B, image 340 KB)");
    expect(auditLine({ at: 1, event: "file_to_desktop", name: "report.pdf", bytes: 2_202_009 } as never)).toBe("File → her desktop: report.pdf (2.1 MB)");

  });
});

describe("ComputerView with LucinaDesk", () => {
  const sockets: string[][] = [];
  beforeEach(() => {
    sockets.length = 0;
    // The owner chose LucinaDesk on this device (Cua's viewer is the default).
    localStorage.setItem("computer.classic", "no");
    // A browser with WebCodecs: LucinaDesk can be the Live view.
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

  it("defaults to Cua's viewer, with LucinaDesk one click away", async () => {
    localStorage.removeItem("computer.classic");
    const calls = fakeLucina(state(), undefined, true);
    renderComputer();
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/computer/view")).toBe(true));
    expect(calls.some((c) => c.url === "/v1/computer/desk")).toBe(false);
    expect(screen.getByRole("button", { name: "Use LucinaDesk" })).toBeInTheDocument();
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
    expect(screen.queryByText("You are watching. Take over to use the mouse and keyboard.")).toBeNull();
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
