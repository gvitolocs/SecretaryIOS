import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { STREAM_BACKOFF_MS } from "../api/timeline";
import { installEventSource, MockEventSource } from "../test/eventsource";
import { deferred, stubFetch, type Call } from "../test/fetch";
import {
  chat,
  cleanMessage,
  clearChat,
  connectTimeline,
  hasMore,
  JOB_POLL_MS,
  loadLatest,
  loadOlder,
  receiveItem,
  reloadChatFromStorage,
  sendMessage,
  streamStatus,
} from "./chat";

const T0 = Date.parse("2026-10-20T10:00:00Z");

function item(id: number, extra: Record<string, unknown> = {}) {
  return { id: String(id), ts: new Date(T0 + id * 60_000).toISOString(), channel: "telegram", role: "user", text: `m${id}`, ...extra };
}

/** Items `from`..`to` (inclusive), newest first, like the server. */
function page(from: number, to: number) {
  const out = [];
  for (let id = to; id >= from; id--) out.push(item(id));
  return out;
}

const ids = () => chat.messages.map((m) => m.itemId ?? m.key);

describe("chat jobs", () => {
  beforeEach(() => reloadChatFromStorage());

  it("polls a job every 3 s until it finishes, then keeps the result", async () => {
    vi.useFakeTimers();
    let polls = 0;
    const calls = stubFetch((c) => {
      if (c.url === "/v1/chat") return { body: { reply: "On it.", action: "task.run", job: { id: "j1", status: "queued" } } };
      polls++;
      return polls < 2
        ? { body: { id: "j1", status: "running", action: "task.run" } }
        : { body: { id: "j1", status: "done", action: "task.run", result: { summary: "All done." } } };
    });

    await sendMessage("do the job");
    const reply = () => chat.messages[1]!;
    expect(reply().job).toMatchObject({ id: "j1", status: "queued" });

    await vi.advanceTimersByTimeAsync(JOB_POLL_MS);
    expect(reply().job?.status).toBe("running");
    await vi.advanceTimersByTimeAsync(JOB_POLL_MS);
    expect(reply().job).toMatchObject({ status: "done", result: { summary: "All done." } });

    await vi.advanceTimersByTimeAsync(JOB_POLL_MS * 3);
    expect(calls.filter((c) => c.url === "/v1/jobs/j1")).toHaveLength(2);
  });

  it("records a job's error and imageUrl", async () => {
    vi.useFakeTimers();
    stubFetch((c) =>
      c.url === "/v1/chat"
        ? { body: { reply: "Drawing.", action: "generate_image", job: { id: "j2", status: "queued" } } }
        : { body: { id: "j2", status: "error", action: "generate_image", result: null, error: "interrupted", imageUrl: null } },
    );
    await sendMessage("draw a cat");
    await vi.advanceTimersByTimeAsync(JOB_POLL_MS);
    expect(chat.messages[1]!.job).toMatchObject({ status: "error", error: "interrupted", action: "generate_image" });
  });

  it("cleans messages to the server's rules", () => {
    expect(cleanMessage("  a\r\nb\u0000c\td\u0007 ")).toBe("a\nbc\td");
  });

  it("does not send over-long messages", async () => {
    const calls = stubFetch(() => ({ body: { reply: "x", job: null } }));
    expect(await sendMessage("x".repeat(4001))).toBe(false);
    expect(calls).toHaveLength(0);
  });

  it("marks interrupted sends as failed after a reload", () => {
    sessionStorage.setItem(
      "secretary.timeline.v1",
      JSON.stringify([{ key: "c:m1", role: "user", text: "hi", createdAt: 1, channel: "web", clientMessageId: "m1", status: "sending" }]),
    );
    reloadChatFromStorage();
    expect(chat.messages[0]).toMatchObject({ status: "error" });
  });

  it("drops the old sessionStorage transcript", () => {
    sessionStorage.setItem("secretary.chat.v1", "[]");
    reloadChatFromStorage();
    expect(sessionStorage.getItem("secretary.chat.v1")).toBeNull();
  });

  it("survives broken sessionStorage", () => {
    const spy = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("QuotaExceeded");
    });
    stubFetch(() => ({ body: { reply: "ok", job: null } }));
    return sendMessage("hello").then(() => {
      expect(chat.messages).toHaveLength(2);
      spy.mockRestore();
    });
  });
});

describe("timeline pages", () => {
  beforeEach(() => reloadChatFromStorage());

  it("loads the newest page, then older ones, merging in time order without duplicates", async () => {
    const calls = stubFetch((c) => {
      if (c.url === "/v1/timeline?limit=50") return { body: { items: page(6, 10), nextBefore: "6" } };
      // The older page overlaps by one item (6) and carries a malformed one.
      if (c.url === "/v1/timeline?before=6&limit=50") return { body: { items: [...page(2, 6), { id: "x" }], nextBefore: null } };
      return { status: 404 };
    });
    await loadLatest();
    expect(ids()).toEqual(["6", "7", "8", "9", "10"]);
    expect(hasMore()).toBe(true);

    expect(await loadOlder()).toBe(4);
    expect(ids()).toEqual(["2", "3", "4", "5", "6", "7", "8", "9", "10"]);
    expect(hasMore()).toBe(false);
    expect(await loadOlder()).toBe(0);
    expect(calls).toHaveLength(2);
  });

  it("merges a refresh into what is already loaded", async () => {
    let newest = 10;
    stubFetch(() => ({ body: { items: page(newest - 4, newest), nextBefore: String(newest - 4) } }));
    await loadLatest();
    newest = 12;
    await loadLatest();
    expect(ids()).toEqual(["6", "7", "8", "9", "10", "11", "12"]);
    // The older cursor still points below what we hold.
    expect(chat.nextBefore).toBe("6");
  });

  it("drops stale history when a refresh no longer joins up with it", async () => {
    let from = 1;
    stubFetch(() => ({ body: { items: page(from, from + 2), nextBefore: String(from) } }));
    await loadLatest();
    from = 100;
    await loadLatest();
    expect(ids()).toEqual(["100", "101", "102"]);
    expect(chat.nextBefore).toBe("100");
  });

  it("ignores live items it already has", () => {
    receiveItem({ id: "5", ts: T0, channel: "discord", role: "user", text: "once" });
    receiveItem({ id: "5", ts: T0, channel: "discord", role: "user", text: "once (edited)" });
    expect(chat.messages).toHaveLength(1);
    expect(chat.messages[0]!.text).toBe("once (edited)");
  });

  it("caches only the last 50 messages", async () => {
    stubFetch(() => ({ body: { items: page(1, 80), nextBefore: null } }));
    await loadLatest();
    const cached = JSON.parse(sessionStorage.getItem("secretary.timeline.v1") ?? "[]") as { itemId: string }[];
    expect(cached).toHaveLength(50);
    expect(cached[49]!.itemId).toBe("80");
  });

  it("drops a page that lands after logout", async () => {
    const gate = deferred<void>();
    stubFetch(async () => {
      await gate.promise;
      return { body: { items: page(1, 3), nextBefore: null } };
    });
    const pending = loadLatest();
    clearChat();
    gate.resolve();
    await pending;
    expect(chat.messages).toHaveLength(0);
  });
});

describe("optimistic sends", () => {
  beforeEach(() => reloadChatFromStorage());

  function chatBody(calls: Call[]) {
    return calls.find((c) => c.url === "/v1/chat")!;
  }

  it("swaps in the server items when the stream delivers them before the reply", async () => {
    const gate = deferred<void>();
    const calls = stubFetch(async () => {
      await gate.promise;
      return { body: { reply: "Hi!", action: "reply", job: null, userItemId: "u1", lucinaItemId: "l1" } };
    });
    const sent = sendMessage("hello");
    const optimistic = chat.messages[0]!;
    expect(optimistic).toMatchObject({ role: "user", status: "sending", channel: "web" });
    const cmid = optimistic.clientMessageId!;
    expect(chatBody(calls).headers["x-secretary-client"]).toBe("web");

    receiveItem({ id: "u1", ts: T0, channel: "web", role: "user", text: "hello", clientMessageId: cmid });
    receiveItem({ id: "l1", ts: T0 + 1000, channel: "web", role: "lucina", text: "Hi!", replyTo: "u1" });
    expect(chat.messages).toHaveLength(2);
    // Still waiting for Lucina's answer to the POST.
    expect(chat.messages[0]).toMatchObject({ key: `c:${cmid}`, itemId: "u1", status: "sending" });

    gate.resolve();
    await sent;
    expect(chat.messages).toHaveLength(2);
    expect(chat.messages.map((m) => [m.role, m.itemId])).toEqual([
      ["user", "u1"],
      ["lucina", "l1"],
    ]);
    expect(chat.messages[0]!.status).toBe("sent");

    // A late replay of the same items changes nothing.
    receiveItem({ id: "u1", ts: T0, channel: "web", role: "user", text: "hello", clientMessageId: cmid });
    receiveItem({ id: "l1", ts: T0 + 1000, channel: "web", role: "lucina", text: "Hi!" });
    expect(chat.messages).toHaveLength(2);
  });

  it("matches the stream's copies when the reply comes first", async () => {
    stubFetch(() => ({ body: { reply: "Hi!", action: "reply", job: null, userItemId: "u1", lucinaItemId: "l1" } }));
    await sendMessage("hello");
    const keys = chat.messages.map((m) => m.key);
    receiveItem({ id: "l1", ts: T0 + 1000, channel: "web", role: "lucina", text: "Hi!" });
    receiveItem({ id: "u1", ts: T0, channel: "web", role: "user", text: "hello" });
    expect(chat.messages.map((m) => m.key)).toEqual(keys);
    expect(chat.messages.map((m) => m.itemId)).toEqual(["u1", "l1"]);
  });

  it("folds in a copy that arrived without a clientMessageId", async () => {
    const gate = deferred<void>();
    stubFetch(async () => {
      await gate.promise;
      return { body: { reply: "Hi!", job: null, userItemId: "u1", lucinaItemId: "l1" } };
    });
    const sent = sendMessage("hello");
    receiveItem({ id: "u1", ts: T0, channel: "web", role: "user", text: "hello" });
    expect(chat.messages).toHaveLength(2); // Not recognisable yet.
    gate.resolve();
    await sent;
    expect(chat.messages.map((m) => m.itemId)).toEqual(["u1", "l1"]);
  });

  it("recognises Lucina's reply by replyTo or clientMessageId when the server sends no ids", async () => {
    stubFetch(() => ({ body: { reply: "Hi!", job: null } }));
    await sendMessage("hello");
    const cmid = chat.messages[0]!.clientMessageId!;
    receiveItem({ id: "u1", ts: T0, channel: "web", role: "user", text: "hello", clientMessageId: cmid });
    receiveItem({ id: "l1", ts: T0 + 1, channel: "web", role: "lucina", text: "Hi!", replyTo: "u1" });
    expect(chat.messages.map((m) => m.itemId)).toEqual(["u1", "l1"]);
  });

  it("keeps the job on Lucina's reply when the stream delivered it first", async () => {
    const gate = deferred<void>();
    stubFetch(async (c) => {
      if (c.url !== "/v1/chat") return { body: { id: "j1", status: "running" } };
      await gate.promise;
      return { body: { reply: "On it.", action: "task", job: { id: "j1", status: "queued" }, userItemId: "u1", lucinaItemId: "l1" } };
    });
    const sent = sendMessage("do a job");
    receiveItem({ id: "l1", ts: Date.now() + 10, channel: "web", role: "lucina", text: "On it." });
    gate.resolve();
    await sent;
    expect(chat.messages).toHaveLength(2);
    expect(chat.messages[1]).toMatchObject({ itemId: "l1", job: { id: "j1" } });
  });

  it("keeps a failed message retryable even if the server recorded it", async () => {
    const gate = deferred<void>();
    stubFetch(async () => {
      await gate.promise;
      return { status: 502, body: { error: { code: "lucina_unreachable", message: "down" } } };
    });
    const sent = sendMessage("hello");
    const cmid = chat.messages[0]!.clientMessageId!;
    receiveItem({ id: "u1", ts: T0, channel: "web", role: "user", text: "hello", clientMessageId: cmid });
    gate.resolve();
    await sent;
    expect(chat.messages).toHaveLength(1);
    expect(chat.messages[0]).toMatchObject({ itemId: "u1", status: "error" });
  });
});

describe("live stream", () => {
  let disconnect: (() => void) | null = null;
  beforeEach(() => reloadChatFromStorage());
  afterEach(() => {
    disconnect?.();
    disconnect = null;
  });

  it("goes live, takes items, and lets the browser handle a dropped connection", async () => {
    installEventSource();
    const calls = stubFetch(() => ({ body: { items: [], nextBefore: null } }));
    disconnect = connectTimeline();
    const es = MockEventSource.latest();
    expect(es.url).toBe("/v1/timeline/stream");
    expect(es.withCredentials).toBe(true);
    expect(streamStatus()).toBe("connecting");

    es.open();
    expect(streamStatus()).toBe("live");
    es.emit("item", item(1));
    es.emit("item", "not json");
    es.emit("item", { id: 2 }); // Malformed: no ts or role.
    expect(ids()).toEqual(["1"]);

    es.drop();
    expect(streamStatus()).toBe("reconnecting");
    es.open(); // The browser reconnected with Last-Event-ID; no resync needed.
    expect(streamStatus()).toBe("live");
    expect(MockEventSource.instances).toHaveLength(1);
    await vi.waitFor(() => expect(calls).toHaveLength(1));
  });

  it("reopens a closed stream with backoff and resyncs the timeline", async () => {
    vi.useFakeTimers();
    installEventSource();
    const calls = stubFetch(() => ({ body: { items: page(1, 2), nextBefore: null } }));
    disconnect = connectTimeline();
    await vi.advanceTimersByTimeAsync(0);
    expect(calls).toHaveLength(1);
    const first = MockEventSource.latest();
    first.open();

    first.fail(); // e.g. a 502 from a restarting server.
    expect(first.closed).toBe(true);
    expect(streamStatus()).toBe("reconnecting");
    await vi.advanceTimersByTimeAsync(STREAM_BACKOFF_MS[0] - 1);
    expect(MockEventSource.instances).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(MockEventSource.instances).toHaveLength(2);

    // It fails again: the next wait is longer.
    MockEventSource.latest().fail();
    await vi.advanceTimersByTimeAsync(STREAM_BACKOFF_MS[1] - 1);
    expect(MockEventSource.instances).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(1);
    expect(MockEventSource.instances).toHaveLength(3);

    MockEventSource.latest().open();
    expect(streamStatus()).toBe("live");
    await vi.advanceTimersByTimeAsync(0);
    expect(calls).toHaveLength(2); // The new stream has no Last-Event-ID: fill the gap.
    // Events from the abandoned connection are ignored.
    first.emit("item", item(9));
    expect(ids()).toEqual(["1", "2"]);
  });

  it("closes the stream on disconnect and on logout", () => {
    installEventSource();
    stubFetch(() => ({ body: { items: [], nextBefore: null } }));
    const off = connectTimeline();
    const es = MockEventSource.latest();
    off();
    expect(es.closed).toBe(true);

    disconnect = connectTimeline();
    const es2 = MockEventSource.latest();
    clearChat();
    expect(es2.closed).toBe(true);
  });
});
