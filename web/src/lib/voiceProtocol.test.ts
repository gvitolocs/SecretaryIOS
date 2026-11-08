import { describe, expect, it } from "vitest";
import {
  backoffMs,
  buildStart,
  inputMs,
  MAX_FRAME_BYTES,
  NOTE_MAX_BYTES,
  parseServerMessage,
  splitFrames,
  voiceUrl,
} from "./voiceProtocol";

const parse = (m: unknown) => parseServerMessage(JSON.stringify(m));

describe("parseServerMessage", () => {
  it("reads a bare listening (an old server) and one that echoes the start", () => {
    expect(parse({ type: "listening" })).toEqual({ type: "listening" });
    expect(parse({ type: "listening", purpose: "note", codec: "pcm16", rate: 16000, turn: "t1" })).toEqual({
      type: "listening",
      purpose: "note",
      codec: "pcm16",
      rate: 16000,
      turn: "t1",
    });
  });

  it("reads endpoint, transcript, delta and answer", () => {
    expect(parse({ type: "endpoint" })).toEqual({ type: "endpoint" });
    expect(parse({ type: "transcript", text: "ciao" })).toEqual({ type: "transcript", text: "ciao" });
    expect(parse({ type: "delta", text: "Ci" })).toEqual({ type: "delta", text: "Ci" });
    expect(parse({ type: "answer", text: "Ciao.", replaces: true })).toEqual({ type: "answer", text: "Ciao.", replaces: true });
    expect(parse({ type: "answer", text: "Ciao." })).toEqual({ type: "answer", text: "Ciao.", replaces: false });
  });

  it("reads audio with its rate, defaulting to 24 kHz", () => {
    expect(parse({ type: "audio", rate: 24000 })).toEqual({ type: "audio", rate: 24000 });
    expect(parse({ type: "audio" })).toEqual({ type: "audio", rate: 24000 });
  });

  it("reads note, reply and done for a note and for a command", () => {
    expect(parse({ type: "note", shareId: "s1", userItemId: "u1" })).toEqual({ type: "note", shareId: "s1", userItemId: "u1" });
    expect(parse({ type: "reply", text: "ok", shareId: "s1", itemId: "i1" })).toEqual({ type: "reply", text: "ok", shareId: "s1", itemId: "i1" });
    expect(parse({ type: "reply", text: "ok" })).toEqual({ type: "reply", text: "ok" });
    expect(parse({ type: "done", ms: { stt: 80, total: 900, route: "fast" } })).toEqual({
      type: "done",
      ms: { stt: 80, total: 900, route: "fast" },
    });
    expect(parse({ type: "done", shareId: "s1", ms: { note: 300, reply: 1500, total: 1500 } })).toEqual({
      type: "done",
      shareId: "s1",
      ms: { note: 300, reply: 1500, total: 1500 },
    });
    expect(parse({ type: "done" })).toEqual({ type: "done", ms: {} });
  });

  it("reads errors, with or without a shareId", () => {
    expect(parse({ type: "error", code: "no_speech", message: "x" })).toEqual({ type: "error", code: "no_speech", message: "x" });
    expect(parse({ type: "error", code: "lucina_unavailable", shareId: "s1" })).toEqual({
      type: "error",
      code: "lucina_unavailable",
      shareId: "s1",
    });
    expect(parse({ type: "error" })).toEqual({ type: "error", code: "unknown" });
  });

  it("ignores unknown types, malformed JSON and incomplete messages", () => {
    expect(parse({ type: "warp" })).toBeNull();
    expect(parse({ nope: 1 })).toBeNull();
    expect(parse([1, 2])).toBeNull();
    expect(parseServerMessage("not json")).toBeNull();
    expect(parseServerMessage("null")).toBeNull();
    expect(parse({ type: "transcript" })).toBeNull();
    expect(parse({ type: "delta", text: 4 })).toBeNull();
    expect(parse({ type: "note", shareId: "s1" })).toBeNull();
    expect(parse({ type: "reply" })).toBeNull();
  });
});

describe("buildStart", () => {
  it("builds the chat note start", () => {
    expect(
      buildStart({ mode: "ptt", purpose: "note", thread: "main", source: "web-chat", sessionId: "abc-1", replyTo: "it_9", lang: "it" }),
    ).toEqual({
      type: "start",
      mode: "ptt",
      purpose: "note",
      codec: "pcm16",
      rate: 16000,
      thread: "main",
      channel: "web",
      source: "web-chat",
      sessionId: "abc-1",
      replyTo: "it_9",
      lang: "it",
    });
  });

  it("builds the stage command start with text, and speak only when false", () => {
    const talk = buildStart({ mode: "vad", purpose: "command", thread: "main", text: true, speak: true });
    expect(talk).toMatchObject({ purpose: "command", mode: "vad", text: true });
    expect(talk).not.toHaveProperty("speak");
    expect(talk).not.toHaveProperty("source");
    expect(buildStart({ mode: "vad", purpose: "command", thread: "main", text: true, speak: false })).toMatchObject({ speak: false });
  });

  it("drops values the server would refuse", () => {
    const m = buildStart({ mode: "ptt", purpose: "note", thread: "main", source: "Web Chat!", sessionId: "bad id", replyTo: "no spaces" });
    expect(m).not.toHaveProperty("source");
    expect(m).not.toHaveProperty("sessionId");
    expect(m).not.toHaveProperty("replyTo");
  });

  it("keeps replyTo and source out of a command", () => {
    const m = buildStart({ mode: "vad", purpose: "command", thread: "main", replyTo: "x", source: "web-chat" });
    expect(m).not.toHaveProperty("replyTo");
    expect(m).not.toHaveProperty("source");
  });
});

describe("frames and units", () => {
  it("passes small frames and cuts big ones on sample boundaries", () => {
    const small = new ArrayBuffer(1920);
    expect(splitFrames(small)).toEqual([small]);
    const parts = splitFrames(new ArrayBuffer(20_000));
    expect(parts.map((p) => p.byteLength)).toEqual([8192, 8192, 3616]);
    expect(parts.every((p) => p.byteLength <= MAX_FRAME_BYTES && p.byteLength % 2 === 0)).toBe(true);
    expect(splitFrames(new ArrayBuffer(9), 5).map((p) => p.byteLength)).toEqual([4, 4, 1]);
  });

  it("counts audio time and the note cap", () => {
    expect(inputMs(32_000)).toBe(1000);
    expect(inputMs(1920)).toBe(60);
    expect(NOTE_MAX_BYTES).toBe(10 * 60 * 32_000);
  });

  it("backs off 1, 2, 4 ... capped at 30 s", () => {
    expect([0, 1, 2, 3, 4, 5, 6, 10].map(backoffMs)).toEqual([1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000]);
  });

  it("uses wss on https and ws on http, on the same host", () => {
    expect(voiceUrl({ protocol: "https:", host: "getsecretary.tech" })).toBe("wss://getsecretary.tech/v1/voice/ws");
    expect(voiceUrl({ protocol: "http:", host: "localhost:5173" })).toBe("ws://localhost:5173/v1/voice/ws");
  });
});
