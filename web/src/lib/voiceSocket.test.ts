import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { pcm, rig } from "../test/voiceWs";
import type { VoiceTurnEvent } from "./voiceProtocol";
import { IDLE_CLOSE_MS, MAX_PENDING_NOTES } from "./voiceSocket";

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

const NOTE = { mode: "ptt", purpose: "note", thread: "main", source: "web-chat" } as const;
const CMD = { mode: "vad", purpose: "command", thread: "main", text: true, speak: true } as const;

function collect() {
  const events: VoiceTurnEvent[] = [];
  return { events, on: (e: VoiceTurnEvent) => void events.push(e), types: () => events.map((e) => e.type) };
}

describe("connecting and reconnecting", () => {
  it("opens on the first voice UI and shares the socket", () => {
    const r = rig();
    const a = r.vs.acquire();
    const b = r.vs.acquire();
    expect(r.sockets).toHaveLength(1);
    expect(r.last().url).toBe("wss://example.test/v1/voice/ws");
    expect(r.last().binaryType).toBe("arraybuffer");
    expect(r.vs.status).toBe("connecting");
    r.last().open();
    expect(r.vs.status).toBe("open");
    a();
    b();
  });

  it("reconnects with backoff 1, 2, 4 ... 30 s and starts over after an open", () => {
    const r = rig();
    r.vs.acquire();
    for (const wait of [1000, 2000, 4000, 8000, 16000, 30000, 30000]) {
      const n = r.sockets.length;
      r.last().drop();
      expect(r.vs.status).toBe("closed");
      vi.advanceTimersByTime(wait - 1);
      expect(r.sockets).toHaveLength(n);
      vi.advanceTimersByTime(1);
      expect(r.sockets).toHaveLength(n + 1);
    }
    r.last().open();
    const n = r.sockets.length;
    r.last().drop();
    vi.advanceTimersByTime(999);
    expect(r.sockets).toHaveLength(n);
    vi.advanceTimersByTime(1);
    expect(r.sockets).toHaveLength(n + 1);
  });

  it("waits 1 s before the first retry", () => {
    const r = rig();
    r.vs.acquire();
    r.last().drop();
    vi.advanceTimersByTime(999);
    expect(r.sockets).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(r.sockets).toHaveLength(2);
  });

  it("closes 60 s after the last voice UI went, and not if one comes back", () => {
    const r = rig();
    const release = r.vs.acquire();
    r.last().open();
    release();
    vi.advanceTimersByTime(IDLE_CLOSE_MS - 1);
    expect(r.last().closed).toBe(false);
    const again = r.vs.acquire();
    vi.advanceTimersByTime(IDLE_CLOSE_MS * 2);
    expect(r.last().closed).toBe(false);
    expect(r.sockets).toHaveLength(1);
    again();
    vi.advanceTimersByTime(IDLE_CLOSE_MS);
    expect(r.last().closed).toBe(true);
    expect(r.vs.status).toBe("closed");
    // No reconnect once nobody wants it.
    vi.advanceTimersByTime(60_000);
    expect(r.sockets).toHaveLength(1);
  });

  it("does not reconnect after the last release", () => {
    const r = rig();
    const release = r.vs.acquire();
    r.last().drop();
    release();
    vi.advanceTimersByTime(60_000);
    expect(r.sockets).toHaveLength(1);
  });
});

describe("one turn at a time", () => {
  it("refuses a turn while not open, and while another runs", () => {
    const r = rig();
    r.vs.acquire();
    expect(r.vs.startTurn(NOTE, () => {})).toBeNull();
    r.last().open();
    const t = r.vs.startTurn(NOTE, () => {});
    expect(t).not.toBeNull();
    expect(r.vs.busy).toBe(true);
    expect(r.vs.startTurn(CMD, () => {})).toBeNull();
  });
});

describe("old server fallback", () => {
  it("cancels and reports unsupported when listening has no codec", () => {
    const r = rig();
    const ws = r.connect();
    const c = collect();
    const turn = r.vs.startTurn(NOTE, c.on)!;
    ws.say({ type: "listening" });
    expect(ws.types).toEqual(["start", "cancel"]);
    expect(c.types()).toEqual(["unsupported"]);
    expect(r.vs.oldServer).toBe(true);
    expect(turn.state).toBe("finished");
    expect(r.vs.busy).toBe(false);
  });

  it("is not an old server when listening echoes the codec", () => {
    const r = rig();
    const ws = r.connect();
    const c = collect();
    r.vs.startTurn(NOTE, c.on);
    ws.say({ type: "listening", purpose: "note", codec: "pcm16", rate: 16000, turn: "t1" });
    expect(ws.types).toEqual(["start"]);
    expect(r.vs.oldServer).toBe(false);
    expect(c.types()).toEqual(["listening"]);
  });

  it("forgets the verdict on a new connection (the server may have been updated)", () => {
    const r = rig();
    const ws = r.connect();
    r.vs.startTurn(NOTE, () => {});
    ws.say({ type: "listening" });
    expect(r.vs.oldServer).toBe(true);
    ws.drop();
    vi.advanceTimersByTime(1000);
    r.last().open();
    expect(r.vs.oldServer).toBe(false);
  });
});

describe("a voice note", () => {
  it("streams frames, ends, frees the device at `note` and follows the reply by shareId", () => {
    const r = rig();
    const ws = r.connect();
    const c = collect();
    const turn = r.vs.startTurn({ ...NOTE, sessionId: "sess1", replyTo: "item7", lang: "it" }, c.on)!;
    expect(ws.json[0]).toMatchObject({
      type: "start",
      mode: "ptt",
      purpose: "note",
      codec: "pcm16",
      rate: 16000,
      thread: "main",
      channel: "web",
      source: "web-chat",
      sessionId: "sess1",
      replyTo: "item7",
      lang: "it",
    });
    ws.say({ type: "listening", purpose: "note", codec: "pcm16", rate: 16000, turn: "t1" });
    turn.audio(pcm(1920));
    turn.audio(pcm(1920));
    expect(ws.frames.map((f) => f.byteLength)).toEqual([1920, 1920]);

    r.clock.t = 1000;
    turn.end();
    expect(ws.types).toEqual(["start", "end"]);
    turn.audio(pcm(1920)); // after `end` nothing is sent
    expect(ws.frames).toHaveLength(2);

    r.clock.t = 1090;
    ws.say({ type: "transcript", text: "buongiorno" });
    r.clock.t = 1400;
    ws.say({ type: "note", shareId: "s1", userItemId: "u1" });
    expect(turn.state).toBe("released");
    expect(r.vs.busy).toBe(false);

    // The device is free: a new turn may start while the first answer is awaited.
    const c2 = collect();
    const next = r.vs.startTurn(CMD, c2.on);
    expect(next).not.toBeNull();

    r.clock.t = 3000;
    ws.say({ type: "reply", shareId: "s1", text: "Buongiorno!", itemId: "i9" });
    ws.say({ type: "done", shareId: "s1", ms: { stt: 80, note: 400, reply: 2000, total: 2000 } });
    expect(c.types()).toEqual(["listening", "transcript", "note", "reply", "done"]);
    expect(c2.events).toEqual([]);
    expect(turn.state).toBe("finished");
    expect(turn.timings).toEqual({ transcript: 90, note: 400, reply: 2000 });
  });

  it("stops at the 10 minute cap and says so", () => {
    const r = rig();
    const ws = r.connect();
    const c = collect();
    const turn = r.vs.startTurn(NOTE, c.on)!;
    const second = 32_000;
    for (let s = 0; s < 599; s++) turn.audio(pcm(second));
    expect(c.types()).toEqual([]);
    turn.audio(pcm(second + 1920));
    expect(c.types()).toEqual(["capped"]);
    expect(ws.types).toEqual(["start", "end"]);
    expect(ws.framedBytes).toBe(600 * second);
    turn.audio(pcm(1920));
    expect(ws.framedBytes).toBe(600 * second);
  });

  it("cuts a big chunk into frames of at most 8 KiB", () => {
    const r = rig();
    const ws = r.connect();
    const turn = r.vs.startTurn(NOTE, () => {})!;
    turn.audio(pcm(20_000));
    expect(ws.frames.map((f) => f.byteLength)).toEqual([8192, 8192, 3616]);
  });

  it("an error before `note` ends the turn; note_failed is one", () => {
    const r = rig();
    const ws = r.connect();
    const c = collect();
    const turn = r.vs.startTurn(NOTE, c.on)!;
    turn.end();
    ws.say({ type: "error", code: "note_failed", message: "share refused: 413" });
    expect(c.events.at(-1)).toMatchObject({ type: "error", code: "note_failed" });
    expect(turn.state).toBe("finished");
    expect(r.vs.busy).toBe(false);
  });

  it("an error after `note` carries the shareId and is not the next turn's", () => {
    const r = rig();
    const ws = r.connect();
    const c = collect();
    const turn = r.vs.startTurn(NOTE, c.on)!;
    turn.end();
    ws.say({ type: "note", shareId: "s1", userItemId: "u1" });
    const c2 = collect();
    r.vs.startTurn(CMD, c2.on);
    ws.say({ type: "error", code: "lucina_unavailable", shareId: "s1" });
    expect(c.events.at(-1)).toMatchObject({ type: "error", shareId: "s1" });
    expect(c2.events).toEqual([]);
    expect(r.vs.busy).toBe(true);
  });

  it("cancel before `note` drops it; after `note` it changes nothing", () => {
    const r = rig();
    const ws = r.connect();
    const turn = r.vs.startTurn(NOTE, () => {})!;
    turn.audio(pcm(1920));
    turn.cancel();
    expect(ws.types).toEqual(["start", "cancel"]);
    expect(r.vs.busy).toBe(false);

    const second = r.vs.startTurn(NOTE, () => {})!;
    second.end();
    ws.say({ type: "note", shareId: "s2", userItemId: "u2" });
    const sent = ws.json.length;
    second.cancel();
    expect(ws.json).toHaveLength(sent);
  });

  it("watches at most four notes; the oldest is dropped", () => {
    const r = rig();
    const ws = r.connect();
    const seen = Array.from({ length: MAX_PENDING_NOTES + 1 }, () => collect());
    for (let i = 0; i <= MAX_PENDING_NOTES; i++) {
      const t = r.vs.startTurn(NOTE, seen[i]!.on)!;
      t.end();
      ws.say({ type: "note", shareId: `s${i}`, userItemId: `u${i}` });
    }
    ws.say({ type: "reply", shareId: "s0", text: "x" });
    ws.say({ type: "reply", shareId: "s1", text: "y" });
    expect(seen[0]!.types()).toEqual(["note"]);
    expect(seen[1]!.types()).toEqual(["note", "reply"]);
  });

  it("ignores messages about a note it does not know", () => {
    const r = rig();
    const ws = r.connect();
    const c = collect();
    r.vs.startTurn(CMD, c.on);
    ws.say({ type: "reply", shareId: "zzz", text: "x" });
    ws.say({ type: "done", shareId: "zzz", ms: {} });
    expect(c.events).toEqual([]);
    expect(r.vs.busy).toBe(true);
  });
});

describe("a voice command", () => {
  it("sends the start, streams until the endpoint, then gets text and audio", () => {
    const r = rig();
    const ws = r.connect();
    const c = collect();
    const turn = r.vs.startTurn(CMD, c.on)!;
    expect(ws.json[0]).toMatchObject({ type: "start", mode: "vad", purpose: "command", thread: "main", channel: "web", text: true });
    ws.say({ type: "listening", purpose: "command", codec: "pcm16", rate: 16000, turn: "t1" });
    turn.audio(pcm(1920));
    r.clock.t = 500;
    ws.say({ type: "endpoint" });
    turn.audio(pcm(1920)); // the client stops sending at the endpoint
    expect(ws.frames).toHaveLength(1);
    expect(turn.state).toBe("ended");

    r.clock.t = 580;
    ws.say({ type: "transcript", text: "che ore sono" });
    r.clock.t = 800;
    ws.say({ type: "delta", text: "Sono" });
    ws.say({ type: "delta", text: " le tre." });
    ws.say({ type: "answer", text: "Sono le tre.", replaces: false });
    r.clock.t = 900;
    ws.say({ type: "audio", rate: 24000 });
    ws.audio(4800);
    ws.audio(4800);
    ws.say({ type: "reply", text: "Sono le tre." });
    ws.say({ type: "done", ms: { stt: 80, lucina_first: 300, tts_first: 400, first_audio: 400, total: 1200 } });

    expect(c.types()).toEqual([
      "listening",
      "endpoint",
      "transcript",
      "delta",
      "delta",
      "answer",
      "audio",
      "frame",
      "frame",
      "reply",
      "done",
    ]);
    const frame = c.events.find((e) => e.type === "frame");
    expect(frame).toMatchObject({ type: "frame", rate: 24000 });
    expect(turn.timings).toEqual({ transcript: 80, firstText: 300, firstAudio: 400, reply: 400 });
    expect(turn.state).toBe("finished");
    expect(r.vs.busy).toBe(false);
  });

  it("a second press sends end, and an error leaves the socket open", () => {
    const r = rig();
    const ws = r.connect();
    const c = collect();
    const turn = r.vs.startTurn(CMD, c.on)!;
    turn.end();
    ws.say({ type: "error", code: "no_speech" });
    expect(ws.types).toEqual(["start", "end"]);
    expect(c.events.at(-1)).toMatchObject({ code: "no_speech" });
    expect(r.vs.status).toBe("open");
    expect(r.vs.startTurn(CMD, () => {})).not.toBeNull();
  });

  it("does not hand audio frames to a note", () => {
    const r = rig();
    const ws = r.connect();
    const c = collect();
    r.vs.startTurn(NOTE, c.on);
    ws.audio(960);
    expect(c.events).toEqual([]);
  });

  it("tells the turn when the socket closes under it", () => {
    const r = rig();
    const ws = r.connect();
    const c = collect();
    r.vs.startTurn(CMD, c.on);
    ws.drop();
    expect(c.types()).toEqual(["closed"]);
    expect(r.vs.busy).toBe(false);
  });
});

describe("timing logs", () => {
  it("logs milliseconds only, under the debug flag", () => {
    const log = vi.spyOn(console, "debug").mockImplementation(() => {});
    const on = rig({ debug: () => true });
    const ws = on.connect();
    const turn = on.vs.startTurn(CMD, () => {})!;
    turn.end();
    on.clock.t = 50;
    ws.say({ type: "transcript", text: "secret words" });
    ws.say({ type: "done", ms: { total: 1 } });
    expect(log).toHaveBeenCalledTimes(1);
    const dump = JSON.stringify(log.mock.calls);
    expect(dump).not.toContain("secret words");
    expect(dump).toContain("transcript");

    log.mockClear();
    const off = rig({ debug: () => false });
    const ws2 = off.connect();
    const t2 = off.vs.startTurn(CMD, () => {})!;
    t2.end();
    ws2.say({ type: "done", ms: {} });
    expect(log).not.toHaveBeenCalled();
    log.mockRestore();
  });
});
