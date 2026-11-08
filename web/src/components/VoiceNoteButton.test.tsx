import { fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { pcm, rig } from "../test/voiceWs";
import { setVoiceSocketForTests } from "../lib/voiceSocket";
import { pendingNotes, removePendingNote } from "../state/voiceNotes";
import { replyTarget, setReplyTarget } from "../state/chat";

let push: (pcm: ArrayBuffer, level: number) => void = () => {};
const stopMic = vi.fn();
let micDenied = false;
vi.mock("../lib/micCapture", async (orig) => ({
  ...(await orig<typeof import("../lib/micCapture")>()),
  openMicrophone: vi.fn(async (cb: typeof push) => {
    if (micDenied) throw new Error("denied");
    push = cb;
    return { stop: stopMic };
  }),
}));

import { VoiceNoteButton } from "./VoiceNoteButton";

beforeEach(() => {
  vi.useFakeTimers();
  micDenied = false;
  stopMic.mockClear();
});
afterEach(() => {
  for (const p of pendingNotes()) removePendingNote(p.key);
  setReplyTarget(null);
  setVoiceSocketForTests(null);
});

function setup() {
  const r = rig();
  setVoiceSocketForTests(r.vs);
  const view = render(() => <VoiceNoteButton />);
  const ws = r.last();
  ws.open();
  const button = screen.getByRole("button", { name: /voice note/i });
  return { r, ws, view, button };
}

const chunks = async (n: number) => {
  await vi.advanceTimersByTimeAsync(0);
  for (let i = 0; i < n; i++) push(pcm(1920), 0.1);
};

describe("VoiceNoteButton", () => {
  it("hold to record, let go to send, and the pending bubble shows", async () => {
    setReplyTarget({ id: "item-5", text: "hello" });
    const { ws, button } = setup();
    fireEvent.pointerDown(button, { button: 0, pointerId: 1 });
    expect(ws.json[0]).toMatchObject({ type: "start", purpose: "note", source: "web-chat", replyTo: "item-5" });
    await chunks(10);
    expect(screen.getByRole("status").textContent).toMatch(/Recording/);
    expect(screen.getByRole("status").textContent).toMatch(/0:00/);
    vi.advanceTimersByTime(600);
    fireEvent.pointerUp(button, { pointerId: 1 });
    expect(ws.types).toEqual(["start", "end"]);
    expect(stopMic).toHaveBeenCalled();
    expect(pendingNotes()).toHaveLength(1);
    ws.say({ type: "transcript", text: "ciao" });
    expect(pendingNotes()[0]?.transcript).toBe("ciao");
    ws.say({ type: "note", shareId: "s1", userItemId: "u1" });
    expect(pendingNotes()[0]?.userItemId).toBe("u1");
    expect(replyTarget()?.id).toBe("item-5");
  });

  it("a quick click starts recording and the next click sends it", async () => {
    const { ws, button } = setup();
    fireEvent.pointerDown(button, { button: 0, pointerId: 1 });
    await chunks(10);
    vi.advanceTimersByTime(100);
    fireEvent.pointerUp(button, { pointerId: 1 });
    expect(ws.types).toEqual(["start"]);
    expect(button.getAttribute("aria-pressed")).toBe("true");
    fireEvent.pointerDown(button, { button: 0, pointerId: 2 });
    fireEvent.pointerUp(button, { pointerId: 2 });
    expect(ws.types).toEqual(["start", "end"]);
    expect(button.getAttribute("aria-pressed")).toBe("false");
  });

  it("Escape cancels: nothing is posted", async () => {
    const { ws, button } = setup();
    fireEvent.pointerDown(button, { button: 0, pointerId: 1 });
    await chunks(10);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(ws.types).toEqual(["start", "cancel"]);
    expect(pendingNotes()).toEqual([]);
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("the cancel button cancels", async () => {
    const { ws, button } = setup();
    fireEvent.pointerDown(button, { button: 0, pointerId: 1 });
    await chunks(10);
    fireEvent.click(screen.getByRole("button", { name: /cancel/i }));
    expect(ws.types).toEqual(["start", "cancel"]);
  });

  it("a blocked microphone shows a short message", async () => {
    micDenied = true;
    const { button } = setup();
    fireEvent.pointerDown(button, { button: 0, pointerId: 1 });
    await vi.advanceTimersByTimeAsync(0);
    expect(screen.getByRole("alert").textContent).toMatch(/microphone is blocked/);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("says so when the voice socket is not connected", () => {
    const r = rig();
    setVoiceSocketForTests(r.vs);
    render(() => <VoiceNoteButton />);
    fireEvent.pointerDown(screen.getByRole("button", { name: /voice note/i }), { button: 0, pointerId: 1 });
    expect(screen.getByRole("alert").textContent).toMatch(/isn't connected/);
  });

  it("a keyboard activation toggles", async () => {
    const { ws, button } = setup();
    fireEvent.click(button, { detail: 0 });
    await chunks(10);
    vi.advanceTimersByTime(500);
    fireEvent.click(button, { detail: 0 });
    expect(ws.types).toEqual(["start", "end"]);
  });

  it("closes the socket 60 s after the button goes", () => {
    const { r, ws, view } = setup();
    view.unmount();
    vi.advanceTimersByTime(59_000);
    expect(ws.closed).toBe(false);
    vi.advanceTimersByTime(1_000);
    expect(ws.closed).toBe(true);
    expect(r.sockets).toHaveLength(1);
  });
});
