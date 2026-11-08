import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { pcm, rig } from "../test/voiceWs";
import type { Microphone } from "./micCapture";
import { failureOf, HOLD_MS, MIN_NOTE_MS, pressKind, recordNote, type NoteCallbacks } from "./voiceNote";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

function mic() {
  let push: (pcm: ArrayBuffer, level: number) => void = () => {};
  const stop = vi.fn();
  const open = vi.fn(async (cb: (pcm: ArrayBuffer, level: number) => void) => {
    push = cb;
    return { stop } as Microphone;
  });
  return { open: open as unknown as typeof import("./micCapture").openMicrophone, stop, push: (b: ArrayBuffer, l = 0.1) => push(b, l), calls: open };
}

function callbacks() {
  const log: string[] = [];
  const cb: NoteCallbacks = {
    onProgress: (ms) => log.push(`progress:${ms}`),
    onTranscript: (t) => log.push(`transcript:${t}`),
    onNote: (n) => log.push(`note:${n.userItemId}`),
    onSettled: () => log.push("settled"),
    onStopped: () => log.push("stopped"),
    onFailure: (w) => log.push(`fail:${w}`),
    onCapped: () => log.push("capped"),
  };
  return { cb, log };
}

describe("press kinds", () => {
  it("a short press toggles, a long one is hold-to-record", () => {
    expect(pressKind(0, HOLD_MS - 1)).toBe("toggle");
    expect(pressKind(0, HOLD_MS)).toBe("hold");
  });

  it("maps server error codes", () => {
    expect(failureOf("no_speech")).toBe("noSpeech");
    expect(failureOf("rate_limited")).toBe("busy");
    expect(failureOf("note_failed")).toBe("failed");
    expect(failureOf("whatever")).toBe("failed");
  });
});

describe("recordNote", () => {
  it("records, sends on release, shows the transcript early and settles on done", async () => {
    const r = rig();
    const ws = r.connect();
    const m = mic();
    const { cb, log } = callbacks();
    const rec = recordNote({ socket: r.vs, openMicrophone: m.open }, { sessionId: "s1", replyTo: "it1", lang: "en" }, cb);
    expect(typeof rec).toBe("object");
    if (typeof rec === "string") return;
    await Promise.resolve();
    expect(m.calls).toHaveBeenCalledWith(expect.any(Function), { chunkMs: 60 });
    expect(ws.json[0]).toMatchObject({ type: "start", purpose: "note", source: "web-chat", replyTo: "it1", sessionId: "s1", lang: "en" });

    for (let i = 0; i < 10; i++) m.push(pcm(1920)); // 600 ms
    expect(rec.ms).toBe(600);
    expect(ws.frames).toHaveLength(10);
    rec.finish();
    expect(ws.types).toEqual(["start", "end"]);
    expect(m.stop).toHaveBeenCalledTimes(1);

    ws.say({ type: "transcript", text: "ciao" });
    ws.say({ type: "note", shareId: "s1", userItemId: "u1" });
    ws.say({ type: "reply", shareId: "s1", text: "Ciao!", itemId: "i1" });
    ws.say({ type: "done", shareId: "s1", ms: {} });
    expect(log.filter((l) => !l.startsWith("progress"))).toEqual(["stopped", "transcript:ciao", "note:u1", "settled"]);
  });

  it("a note shorter than the minimum is dropped, nothing posted", async () => {
    const r = rig();
    const ws = r.connect();
    const m = mic();
    const { cb, log } = callbacks();
    const rec = recordNote({ socket: r.vs, openMicrophone: m.open }, {}, cb);
    if (typeof rec === "string") throw new Error(rec);
    await Promise.resolve();
    m.push(pcm(1920));
    expect(rec.ms).toBeLessThan(MIN_NOTE_MS);
    rec.finish();
    expect(ws.types).toEqual(["start", "cancel"]);
    expect(log).toContain("fail:tooShort");
  });

  it("cancel (Esc) drops the recording quietly", async () => {
    const r = rig();
    const ws = r.connect();
    const m = mic();
    const { cb, log } = callbacks();
    const rec = recordNote({ socket: r.vs, openMicrophone: m.open }, {}, cb);
    if (typeof rec === "string") throw new Error(rec);
    await Promise.resolve();
    for (let i = 0; i < 10; i++) m.push(pcm(1920));
    rec.cancel();
    expect(ws.types).toEqual(["start", "cancel"]);
    expect(m.stop).toHaveBeenCalled();
    expect(log.filter((l) => l.startsWith("fail"))).toEqual([]);
    expect(log).toContain("stopped");
    m.push(pcm(1920)); // late chunks go nowhere
    expect(ws.frames).toHaveLength(10);
  });

  it("a blocked microphone cancels the turn and says so", async () => {
    const r = rig();
    const ws = r.connect();
    const { cb, log } = callbacks();
    const rec = recordNote({ socket: r.vs, openMicrophone: (async () => Promise.reject(new Error("denied"))) as never }, {}, cb);
    expect(typeof rec).toBe("object");
    await vi.advanceTimersByTimeAsync(0);
    expect(ws.types).toEqual(["start", "cancel"]);
    expect(log).toEqual(["stopped", "fail:micBlocked"]);
  });

  it("closes a microphone that opened after the user already let go", async () => {
    const r = rig();
    r.connect();
    const stop = vi.fn();
    let resolve: (m: Microphone) => void = () => {};
    const open = (() => new Promise<Microphone>((res) => (resolve = res))) as never;
    const { cb } = callbacks();
    const rec = recordNote({ socket: r.vs, openMicrophone: open }, {}, cb);
    if (typeof rec === "string") throw new Error(rec);
    rec.cancel();
    resolve({ stop });
    await Promise.resolve();
    expect(stop).toHaveBeenCalledTimes(1);
  });

  it("does not start offline, busy, or against an old server", () => {
    const r = rig();
    const { cb } = callbacks();
    r.vs.acquire();
    expect(recordNote({ socket: r.vs, openMicrophone: mic().open }, {}, cb)).toBe("offline");
    r.last().open();
    const first = recordNote({ socket: r.vs, openMicrophone: mic().open }, {}, cb);
    expect(typeof first).toBe("object");
    expect(recordNote({ socket: r.vs, openMicrophone: mic().open }, {}, cb)).toBe("busy");
    r.last().say({ type: "listening" });
    expect(recordNote({ socket: r.vs, openMicrophone: mic().open }, {}, cb)).toBe("oldServer");
  });

  it("reports an old server that answers after the press", async () => {
    const r = rig();
    const ws = r.connect();
    const m = mic();
    const { cb, log } = callbacks();
    recordNote({ socket: r.vs, openMicrophone: m.open }, {}, cb);
    await Promise.resolve();
    ws.say({ type: "listening" });
    expect(ws.types).toEqual(["start", "cancel"]);
    expect(log).toEqual(["stopped", "fail:oldServer"]);
    expect(m.stop).toHaveBeenCalled();
  });

  it("reports the 10 minute cap", async () => {
    const r = rig();
    const ws = r.connect();
    const m = mic();
    const { cb, log } = callbacks();
    recordNote({ socket: r.vs, openMicrophone: m.open }, {}, cb);
    await Promise.resolve();
    for (let s = 0; s < 600; s++) m.push(pcm(32_000));
    expect(ws.types).toEqual(["start", "end"]);
    expect(log).toContain("capped");
    expect(m.stop).toHaveBeenCalled();
  });

  it("an offline drop during recording is reported once", async () => {
    const r = rig();
    const ws = r.connect();
    const m = mic();
    const { cb, log } = callbacks();
    recordNote({ socket: r.vs, openMicrophone: m.open }, {}, cb);
    await Promise.resolve();
    ws.drop();
    expect(log.filter((l) => l.startsWith("fail"))).toEqual(["fail:offline"]);
    expect(log.filter((l) => l === "stopped")).toHaveLength(1);
  });
});
