import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ControlState } from "../api/computer";
import { encodeVideo } from "../lib/deskWire";

const ticket = vi.fn();
vi.mock("../api/desk", () => ({ deskApi: { ticket: (...a: unknown[]) => ticket(...a) } }));

import DeskPlayer from "./DeskPlayer";

// A WebSocket the test drives by hand.
class FakeWS {
  static all: FakeWS[] = [];
  static OPEN = 1;
  readyState = 0;
  binaryType = "";
  sent: unknown[] = [];
  closedWith: number | null = null;
  onopen: (() => void) | null = null;
  onmessage: ((m: { data: unknown }) => void) | null = null;
  onclose: ((e: { code: number; reason: string }) => void) | null = null;
  constructor(
    readonly url: string,
    readonly protocols: string[],
  ) {
    FakeWS.all.push(this);
  }
  send(d: string) {
    this.sent.push(JSON.parse(d));
  }
  close(code = 1000) {
    this.closedWith = code;
    this.readyState = 3;
  }
  open() {
    this.readyState = 1;
    this.onopen?.();
  }
  frame(key = true, last = true, pts = 1) {
    this.onmessage?.({ data: encodeVideo({ codec: "vp9", key, last, display: 0, pts, data: new Uint8Array([1, 2]) }) });
  }
  text(m: unknown) {
    this.onmessage?.({ data: JSON.stringify(m) });
  }
  drop(code: number, reason = "") {
    this.readyState = 3;
    this.onclose?.({ code, reason });
  }
}

// WebCodecs: decoding is immediate and yields a 1280x800 frame with the chunk's timestamp.
class FakeDecoder {
  state = "unconfigured";
  decodeQueueSize = 0;
  constructor(private readonly init: { output: (f: unknown) => void; error: (e: unknown) => void }) {}
  configure() {
    this.state = "configured";
  }
  decode(chunk: { timestamp: number }) {
    this.init.output({ displayWidth: 1280, displayHeight: 800, timestamp: chunk.timestamp, close() {} });
  }
  close() {
    this.state = "closed";
  }
  static isConfigSupported = vi.fn(async (c: { codec: string }) => ({ supported: c.codec.startsWith("vp09") || c.codec.startsWith("avc1") }));
}
class FakeChunk {
  timestamp: number;
  constructor(i: { timestamp: number }) {
    this.timestamp = i.timestamp;
  }
}

const T = (mode: "watch" | "control") => ({
  ticket: `t-${mode}-${Math.random()}`,
  protocol: "lucinadesk.v1",
  wsPath: "/v1/computer/desk/ws",
  expiresInSecs: 30,
  sandbox: "workspace",
  mode,
  lan: null,
});

const idle: ControlState = { sandbox: "workspace", driver: "none", epoch: 1, owner: null, task: null, lastAgent: null, leaseSecs: 45 };
const busy: ControlState = { ...idle, driver: "lucina", task: { who: "lucina", what: "book a table", since: 0, stopping: false } };

/** Pretend to run inside Secretary Code (lib/focusMode reads `window.secretaryShell`). */
const shellIs = (s: object) => ((window as unknown as { secretaryShell?: unknown }).secretaryShell = s);

const LOGIN = { type: "login", hostname: "ws", platform: "Linux", version: "1.4.9", currentDisplay: 0, displays: [{ x: 0, y: 0, width: 1280, height: 800 }] };

async function live(ws: FakeWS) {
  ws.open();
  ws.text({ type: "hello", v: 1, mode: "watch", sandbox: "workspace" });
  ws.text(LOGIN);
  ws.frame();
}

beforeEach(() => {
  FakeWS.all = [];
  ticket.mockReset();
  ticket.mockImplementation(async (_s: string, mode: "watch" | "control") => T(mode));
  vi.stubGlobal("WebSocket", FakeWS);
  vi.stubGlobal("VideoDecoder", FakeDecoder);
  vi.stubGlobal("EncodedVideoChunk", FakeChunk);
  HTMLCanvasElement.prototype.getContext = (() => ({ drawImage() {} })) as never;
  // jsdom lays nothing out: give the canvas a box so clicks map to host pixels.
  HTMLCanvasElement.prototype.getBoundingClientRect = () => ({ left: 0, top: 0, width: 640, height: 400, right: 640, bottom: 400, x: 0, y: 0, toJSON() {} }) as DOMRect;
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("DeskPlayer", () => {
  it("watches: a watch ticket with this device's codecs, the subprotocols, and acks after decoding", async () => {
    localStorage.setItem("desk.codecs", "vp9,h264");
    render(() => <DeskPlayer sandbox="workspace" state={idle} onFallback={() => undefined} />);
    await waitFor(() => expect(FakeWS.all).toHaveLength(1));
    expect(ticket).toHaveBeenCalledWith("workspace", "watch", ["vp9", "h264"]);
    const ws = FakeWS.all[0]!;
    expect(ws.url.endsWith("/v1/computer/desk/ws")).toBe(true);
    expect(ws.protocols[0]).toBe("lucinadesk.v1");
    expect(ws.protocols[1]).toMatch(/^ticket\.t-watch-/);
    await live(ws);
    // A delta before any key frame would be skipped; this key frame flagged last is decoded and acked.
    expect(ws.sent).toContainEqual({ type: "ack" });
    expect(screen.getByRole("toolbar").textContent).toContain("Watching");
  });

  it("does not wait for the codec probe: a first visit asks with the usual codecs", async () => {
    render(() => <DeskPlayer sandbox="workspace" state={idle} onFallback={() => undefined} />);
    await waitFor(() => expect(FakeWS.all).toHaveLength(1));
    expect(ticket).toHaveBeenCalledWith("workspace", "watch", ["vp9", "av1", "h264"]);
  });

  it("skips frames until a key frame, keeping the host going without asking (the gateway does)", async () => {
    render(() => <DeskPlayer sandbox="workspace" state={idle} onFallback={() => undefined} />);
    await waitFor(() => expect(FakeWS.all).toHaveLength(1));
    const ws = FakeWS.all[0]!;
    ws.open();
    ws.frame(false, true, 5);
    // The gateway asks the host for a key frame on join; we only ack.
    expect(ws.sent).toEqual([{ type: "ack" }]);
    const now = performance.now();
    vi.spyOn(performance, "now").mockReturnValue(now + 1500);
    ws.frame(false, true, 6);
    ws.frame(false, true, 7);
    expect(ws.sent).toEqual([{ type: "ack" }, { type: "ack" }, { type: "ack" }]);
  });

  it("the first click takes over, then is delivered on the control socket; the watch socket goes", async () => {
    const changed = vi.fn();
    render(() => <DeskPlayer sandbox="workspace" state={idle} onFallback={() => undefined} onChanged={changed} />);
    await waitFor(() => expect(FakeWS.all).toHaveLength(1));
    const watch = FakeWS.all[0]!;
    await live(watch);
    const canvas = screen.getByRole("img");
    // The middle of a 640x400 box = host (640, 400).
    fireEvent.pointerDown(canvas, { pointerType: "mouse", button: 0, clientX: 320, clientY: 200 });
    // Watching: nothing reached the watch socket but acks.
    expect(watch.sent.filter((m) => (m as { type: string }).type === "mouse")).toEqual([]);
    await waitFor(() => expect(FakeWS.all).toHaveLength(2));
    expect(ticket).toHaveBeenLastCalledWith("workspace", "control", expect.arrayContaining(["vp9"]));
    const control = FakeWS.all[1]!;
    control.open();
    control.text(LOGIN);
    control.frame();
    expect(watch.closedWith).toBe(1000);
    expect(control.sent).toContainEqual({ type: "mouse", mask: 0, x: 640, y: 400 });
    expect(control.sent).toContainEqual({ type: "mouse", mask: 9, x: 640, y: 400 });
    expect(control.sent).toContainEqual({ type: "mouse", mask: 10, x: 640, y: 400 });
    expect(screen.getByRole("toolbar").textContent).toContain("You're driving");
    expect(changed).toHaveBeenCalled();

    // Keys now go straight to the control socket.
    fireEvent.keyDown(canvas, { key: "Enter" });
    expect(control.sent).toContainEqual({ type: "key", key: "Return" });

    // Give back: a new watch socket, and the control one closes once it shows a frame.
    fireEvent.click(screen.getByRole("button", { name: "Give back" }));
    await waitFor(() => expect(FakeWS.all).toHaveLength(3));
    const back = FakeWS.all[2]!;
    expect(back.protocols[1]).toMatch(/^ticket\.t-watch-/);
    await live(back);
    expect(control.closedWith).toBe(1000);
    expect(screen.getByRole("toolbar").textContent).toContain("Watching");
  });

  it("asks before taking over while Lucina is working, and does not replay that click", async () => {
    render(() => <DeskPlayer sandbox="workspace" state={busy} onFallback={() => undefined} />);
    await waitFor(() => expect(FakeWS.all).toHaveLength(1));
    await live(FakeWS.all[0]!);
    fireEvent.pointerDown(screen.getByRole("img"), { pointerType: "mouse", button: 0, clientX: 10, clientY: 10 });
    const dialog = screen.getByRole("dialog");
    expect(dialog.textContent).toContain("book a table");
    expect(FakeWS.all).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Take over" }));
    await waitFor(() => expect(FakeWS.all).toHaveLength(2));
    const control = FakeWS.all[1]!;
    control.open();
    control.frame();
    expect(control.sent.filter((m) => (m as { type: string }).type === "mouse")).toEqual([]);
  });

  it("falls back to the Cua viewer when the desk endpoint is missing or the gateway is down", async () => {
    const { ApiError } = await import("../api/client");
    ticket.mockRejectedValueOnce(new ApiError(404, "not_found", "no route"));
    const fallback = vi.fn();
    render(() => <DeskPlayer sandbox="workspace" state={idle} onFallback={fallback} />);
    await waitFor(() => expect(fallback).toHaveBeenCalledWith("unavailable"));

    ticket.mockRejectedValueOnce(new ApiError(503, "desk_unavailable", "gateway down"));
    const fallback503 = vi.fn();
    render(() => <DeskPlayer sandbox="workspace" state={idle} onFallback={fallback503} />);
    await waitFor(() => expect(fallback503).toHaveBeenCalledWith("unavailable"));

    ticket.mockImplementation(async (_s: string, mode: "watch" | "control") => T(mode));
    const fallback2 = vi.fn();
    render(() => <DeskPlayer sandbox="workspace" state={idle} onFallback={fallback2} />);
    await waitFor(() => expect(FakeWS.all).toHaveLength(1));
    FakeWS.all[0]!.open();
    FakeWS.all[0]!.drop(4003, "gateway unavailable");
    expect(fallback2).toHaveBeenCalledWith("unavailable");
  });

  it("inside Secretary Code: focus by default, follows the shell leaving it, asks again on the next visit", async () => {
    const calls: boolean[] = [];
    let listener: ((on: boolean) => void) | null = null;
    shellIs({
      showCode() {},
      setFocusMode: (on: boolean) => calls.push(on),
      onFocusModeChange: (l: (on: boolean) => void) => {
        listener = l;
        return () => (listener = null);
      },
    });
    try {
      const { container, unmount } = render(() => <DeskPlayer sandbox="workspace" state={idle} onFallback={() => undefined} />);
      const stage = () => container.querySelector(".ldesk-stage")!;
      await waitFor(() => expect(calls).toEqual([true]));
      expect(stage().classList.contains("ldesk-stage--focus")).toBe(true);
      // Esc in the shell: it already left focus; the page follows and does not echo or fight it.
      listener!(false);
      await waitFor(() => expect(stage().classList.contains("ldesk-stage--focus")).toBe(false));
      expect(calls).toEqual([true]);
      // Back from another view: a new visit, focus again.
      Object.defineProperty(document, "hidden", { configurable: true, get: () => false });
      document.dispatchEvent(new Event("visibilitychange"));
      await waitFor(() => expect(calls).toEqual([true, true]));
      unmount();
      expect(calls).toEqual([true, true, false]);
      expect(listener).toBeNull();
    } finally {
      delete window.secretaryShell;
      delete (document as { hidden?: boolean }).hidden;
    }
  });

  it("a background mount in Secretary Code asks for focus only once it is on screen", async () => {
    const calls: boolean[] = [];
    shellIs({ showCode() {}, setFocusMode: (on: boolean) => calls.push(on), onFocusModeChange: () => () => undefined });
    let state: DocumentVisibilityState = "hidden";
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => state });
    Object.defineProperty(document, "hidden", { configurable: true, get: () => state === "hidden" });
    try {
      const { unmount } = render(() => <DeskPlayer sandbox="workspace" state={idle} onFallback={() => undefined} />);
      await waitFor(() => expect(FakeWS.all).toHaveLength(1));
      expect(calls).toEqual([]);
      state = "visible";
      document.dispatchEvent(new Event("visibilitychange"));
      await waitFor(() => expect(calls).toEqual([true]));
      unmount();
      expect(calls).toEqual([true, false]);
    } finally {
      delete window.secretaryShell;
      delete (document as { visibilityState?: string }).visibilityState;
      delete (document as { hidden?: boolean }).hidden;
    }
  });

  it("leaving while driving gives the desktop back outright", async () => {
    const fetches: { url: string; body: string }[] = [];
    vi.stubGlobal("fetch", async (url: string, init: { body: string }) => {
      fetches.push({ url, body: init.body });
      return new Response(null, { status: 204 });
    });
    const { unmount } = render(() => <DeskPlayer sandbox="workspace" state={idle} onFallback={() => undefined} />);
    await waitFor(() => expect(FakeWS.all).toHaveLength(1));
    await live(FakeWS.all[0]!);
    fireEvent.keyDown(screen.getByRole("img"), { key: "x" });
    await waitFor(() => expect(FakeWS.all).toHaveLength(2));
    FakeWS.all[1]!.open();
    FakeWS.all[1]!.frame();
    unmount();
    expect(fetches).toEqual([{ url: "/v1/computer/control", body: JSON.stringify({ sandbox: "workspace", action: "release" }) }]);
  });

  describe("clipboard", () => {
    let readText: ReturnType<typeof vi.fn>;
    let writeText: ReturnType<typeof vi.fn>;
    beforeEach(() => {
      readText = vi.fn(async () => "from the Mac");
      writeText = vi.fn(async () => undefined);
      Object.defineProperty(navigator, "clipboard", { configurable: true, value: { readText, writeText } });
    });
    afterEach(() => {
      delete (navigator as { clipboard?: unknown }).clipboard;
    });

    /** Watching, then driving through a key that is not ⌘V; returns the control socket. */
    async function drive() {
      render(() => <DeskPlayer sandbox="workspace" state={idle} onFallback={() => undefined} />);
      await waitFor(() => expect(FakeWS.all).toHaveLength(1));
      await live(FakeWS.all[0]!);
      fireEvent.keyDown(screen.getByRole("img"), { key: "Shift" });
      fireEvent.pointerDown(screen.getByRole("img"), { pointerType: "mouse", button: 0, clientX: 1, clientY: 1 });
      await waitFor(() => expect(FakeWS.all).toHaveLength(2));
      const control = FakeWS.all[1]!;
      control.open();
      control.text(LOGIN);
      control.frame();
      control.sent.length = 0;
      return control;
    }
    const nonAcks = (ws: FakeWS) => ws.sent.filter((m) => (m as { type: string }).type !== "ack" && (m as { type: string }).type !== "ping");

    it("⌘V while driving: the Mac's text onto Lucina's clipboard, then Ctrl+V, in that order", async () => {
      const control = await drive();
      fireEvent.keyDown(screen.getByRole("img"), { key: "v", metaKey: true });
      await waitFor(() => expect(nonAcks(control)).toHaveLength(2));
      expect(nonAcks(control)).toEqual([
        { type: "clipboard", text: "from the Mac" },
        { type: "key", char: "v", modifiers: ["Control"] },
      ]);
    });

    it("⌘C while driving is Ctrl+C there; what she copied comes back onto the Mac's clipboard", async () => {
      const control = await drive();
      fireEvent.keyDown(screen.getByRole("img"), { key: "c", metaKey: true });
      expect(nonAcks(control)).toEqual([{ type: "key", char: "c", modifiers: ["Control"] }]);
      control.text({ type: "clipboard", text: "from Lucina" });
      await waitFor(() => expect(writeText).toHaveBeenCalledWith("from Lucina"));
      expect(screen.queryByRole("button", { name: /tap to copy/ })).toBeNull();
    });

    it("where the browser refuses to write, a chip copies on tap", async () => {
      writeText.mockRejectedValueOnce(new Error("not focused"));
      const control = await drive();
      control.text({ type: "clipboard", text: "from Lucina" });
      const chip = await screen.findByRole("button", { name: /tap to copy/ });
      fireEvent.click(chip);
      await waitFor(() => expect(writeText).toHaveBeenCalledTimes(2));
      expect(writeText).toHaveBeenLastCalledWith("from Lucina");
      await waitFor(() => expect(screen.queryByRole("button", { name: /tap to copy/ })).toBeNull());
    });

    it("refuses a paste over 60,000 bytes before sending anything", async () => {
      readText.mockResolvedValueOnce("x".repeat(60_001));
      const control = await drive();
      fireEvent.keyDown(screen.getByRole("img"), { key: "v", metaKey: true });
      expect(await screen.findByText("Too much to paste: the limit is 60 KB of text.")).toBeInTheDocument();
      expect(nonAcks(control)).toEqual([]);
    });

    it("the iPhone's Paste is a real paste on her side too", async () => {
      const control = await drive();
      const kbd = document.querySelector("textarea.ldesk-kbd") as HTMLTextAreaElement;
      kbd.value = " pasted words";
      fireEvent(kbd, new InputEvent("input", { inputType: "insertFromPaste", data: null, bubbles: true }));
      expect(nonAcks(control)).toEqual([
        { type: "clipboard", text: "pasted words" },
        { type: "key", char: "v", modifiers: ["Control"] },
      ]);
    });

    it("watching: no clipboard either way; ⌘V only asks to take over", async () => {
      render(() => <DeskPlayer sandbox="workspace" state={idle} onFallback={() => undefined} />);
      await waitFor(() => expect(FakeWS.all).toHaveLength(1));
      const watch = FakeWS.all[0]!;
      await live(watch);
      watch.text({ type: "clipboard", text: "should not land" });
      fireEvent.keyDown(screen.getByRole("img"), { key: "v", metaKey: true });
      expect(readText).not.toHaveBeenCalled();
      expect(writeText).not.toHaveBeenCalled();
      expect(nonAcks(watch)).toEqual([]);
      await waitFor(() => expect(FakeWS.all).toHaveLength(2));
      const control = FakeWS.all[1]!;
      control.open();
      control.frame();
      // The ⌘V that took over is not replayed as a paste.
      expect(nonAcks(control)).toEqual([]);
      expect(readText).not.toHaveBeenCalled();
    });
  });

  it("when the lease ends elsewhere (4001) it goes back to watching", async () => {
    render(() => <DeskPlayer sandbox="workspace" state={idle} onFallback={() => undefined} />);
    await waitFor(() => expect(FakeWS.all).toHaveLength(1));
    await live(FakeWS.all[0]!);
    fireEvent.keyDown(screen.getByRole("img"), { key: "a" });
    await waitFor(() => expect(FakeWS.all).toHaveLength(2));
    const control = FakeWS.all[1]!;
    control.open();
    control.frame();
    expect(control.sent).toContainEqual({ type: "text", text: "a" });
    control.drop(4001);
    await waitFor(() => expect(FakeWS.all).toHaveLength(3));
    expect(FakeWS.all[2]!.protocols[1]).toMatch(/^ticket\.t-watch-/);
    expect(screen.getByText("Your takeover ended. You are watching again.")).toBeInTheDocument();
  });
});
