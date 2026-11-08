import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { pcm, rig } from "../test/voiceWs";
import type { Microphone } from "./micCapture";
import { nextReplyText, runCommand, type CommandCallbacks } from "./voiceCommand";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

function setup() {
  const r = rig();
  const ws = r.connect();
  let push: (b: ArrayBuffer, l: number) => void = () => {};
  const stop = vi.fn();
  const openMicrophone = (async (cb: typeof push) => {
    push = cb;
    return { stop } as Microphone;
  }) as never;
  const player = { push: vi.fn(), end: vi.fn((cb?: () => void) => cb?.()), stop: vi.fn() };
  const log: string[] = [];
  const cb: CommandCallbacks = {
    onProgress: () => {},
    onTranscript: (t) => log.push(`transcript:${t}`),
    onText: (t) => log.push(`text:${t}`),
    onSpoken: () => log.push("spoken"),
    onFinished: () => log.push("finished"),
    onFailure: (w) => log.push(`fail:${w}`),
    onStopped: () => log.push("stopped"),
  };
  return { r, ws, openMicrophone, player, log, cb, mic: (b: ArrayBuffer) => push(b, 0.2), stop };
}

describe("nextReplyText", () => {
  it("grows with deltas and is replaced by an answer that says so", () => {
    let t = nextReplyText("", { type: "delta", text: "Sono" });
    t = nextReplyText(t, { type: "delta", text: " le tre" });
    expect(t).toBe("Sono le tre");
    expect(nextReplyText(t, { type: "answer", text: "Sono le tre.", replaces: true })).toBe("Sono le tre.");
    expect(nextReplyText(t, { type: "answer", text: "Sono le tre.", replaces: false })).toBe("Sono le tre");
    expect(nextReplyText("", { type: "answer", text: "Ciao.", replaces: false })).toBe("Ciao.");
    expect(nextReplyText("x", { type: "transcript", text: "y" })).toBe("x");
  });
});

describe("runCommand", () => {
  it("streams until the endpoint, shows the text and plays the audio as it comes", async () => {
    const s = setup();
    const run = runCommand({ socket: s.r.vs, player: s.player, openMicrophone: s.openMicrophone }, { speak: true, sessionId: "s1", lang: "it" }, s.cb);
    expect(typeof run).toBe("object");
    await Promise.resolve();
    expect(s.ws.json[0]).toMatchObject({ type: "start", mode: "vad", purpose: "command", thread: "main", channel: "web", text: true, sessionId: "s1", lang: "it" });
    expect(s.ws.json[0]).not.toHaveProperty("speak");
    s.mic(pcm(1920));
    s.mic(pcm(1920));
    expect(s.ws.frames).toHaveLength(2);

    s.ws.say({ type: "endpoint" });
    expect(s.log).toEqual(["stopped", "spoken"]);
    expect(s.stop).toHaveBeenCalled();
    s.mic(pcm(1920));
    expect(s.ws.frames).toHaveLength(2);

    s.ws.say({ type: "transcript", text: "ciao" });
    s.ws.say({ type: "delta", text: "Ci" });
    s.ws.say({ type: "delta", text: "ao" });
    s.ws.say({ type: "answer", text: "Ciao!", replaces: true });
    s.ws.say({ type: "audio", rate: 24000 });
    s.ws.audio(4800);
    s.ws.audio(4800);
    expect(s.player.push).toHaveBeenCalledTimes(2);
    expect(s.player.push).toHaveBeenCalledWith(expect.any(ArrayBuffer), 24000);
    s.ws.say({ type: "reply", text: "Ciao!" });
    s.ws.say({ type: "done", ms: { total: 800 } });
    expect(s.log).toEqual(["stopped", "spoken", "transcript:ciao", "text:Ci", "text:Ciao", "text:Ciao!", "finished"]);
    expect(s.player.end).toHaveBeenCalledTimes(1);
  });

  it("a second press ends the recording", async () => {
    const s = setup();
    const run = runCommand({ socket: s.r.vs, player: s.player, openMicrophone: s.openMicrophone }, { speak: true }, s.cb);
    if (typeof run === "string") throw new Error(run);
    await Promise.resolve();
    run.finish();
    expect(s.ws.types).toEqual(["start", "end"]);
    expect(s.log).toEqual(["stopped", "spoken"]);
    run.finish();
    expect(s.ws.types).toEqual(["start", "end"]);
  });

  it("muted: asks for no speech and plays nothing", async () => {
    const s = setup();
    runCommand({ socket: s.r.vs, player: s.player, openMicrophone: s.openMicrophone }, { speak: false }, s.cb);
    await Promise.resolve();
    expect(s.ws.json[0]).toMatchObject({ speak: false, text: true });
    s.ws.audio(960);
    expect(s.player.push).not.toHaveBeenCalled();
  });

  it("cancel stops the mic, her voice and the turn", async () => {
    const s = setup();
    const run = runCommand({ socket: s.r.vs, player: s.player, openMicrophone: s.openMicrophone }, { speak: true }, s.cb);
    if (typeof run === "string") throw new Error(run);
    await Promise.resolve();
    run.cancel();
    expect(s.ws.types).toEqual(["start", "cancel"]);
    expect(s.player.stop).toHaveBeenCalled();
    expect(s.stop).toHaveBeenCalled();
    s.ws.say({ type: "transcript", text: "late" });
    expect(s.log).toEqual(["stopped"]);
  });

  it("an error stops everything and reports it", async () => {
    const s = setup();
    runCommand({ socket: s.r.vs, player: s.player, openMicrophone: s.openMicrophone }, { speak: true }, s.cb);
    await Promise.resolve();
    s.ws.say({ type: "error", code: "no_speech" });
    expect(s.log).toEqual(["stopped", "fail:noSpeech"]);
    expect(s.player.stop).toHaveBeenCalled();
  });

  it("falls back on an old server", async () => {
    const s = setup();
    runCommand({ socket: s.r.vs, player: s.player, openMicrophone: s.openMicrophone }, { speak: true }, s.cb);
    await Promise.resolve();
    s.ws.say({ type: "listening" });
    expect(s.ws.types).toEqual(["start", "cancel"]);
    expect(s.log).toEqual(["stopped", "fail:oldServer"]);
    expect(runCommand({ socket: s.r.vs, player: s.player, openMicrophone: s.openMicrophone }, { speak: true }, s.cb)).toBe("oldServer");
  });

  it("does not start while offline", () => {
    const r = rig();
    r.vs.acquire();
    const player = { push: vi.fn(), end: vi.fn(), stop: vi.fn() };
    expect(runCommand({ socket: r.vs, player }, { speak: true }, setup().cb)).toBe("offline");
  });
});
