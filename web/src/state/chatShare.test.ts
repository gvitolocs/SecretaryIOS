import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { stubFetch } from "../test/fetch";
import { draftFor } from "../lib/attachments";
import {
  chat,
  clearChat,
  reloadChatFromStorage,
  retryMessage,
  sendAttachmentMessage,
  stopSharePolling,
} from "./chat";

class FakeXHR {
  static instances: FakeXHR[] = [];
  upload: { onprogress?: (event: ProgressEvent) => void } = {};
  status = 0;
  responseText = "";
  withCredentials = false;
  timeout = 0;
  body: FormData | null = null;
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  ontimeout: (() => void) | null = null;
  onabort: (() => void) | null = null;
  open = vi.fn();
  setRequestHeader = vi.fn();
  send = vi.fn((form: FormData) => {
    this.body = form;
  });
  constructor() {
    FakeXHR.instances.push(this);
  }
  progress(loaded: number, total: number) {
    this.upload.onprogress?.({ loaded, total } as ProgressEvent);
  }
  finish(status: number, body: unknown) {
    this.status = status;
    this.responseText = JSON.stringify(body);
    this.onload?.();
  }
}

const file = () => new File(["photo"], "photo.png", { type: "image/png" });

beforeEach(() => {
  clearChat();
  FakeXHR.instances = [];
  vi.stubGlobal("XMLHttpRequest", FakeXHR as unknown as typeof XMLHttpRequest);
});

afterEach(() => {
  stopSharePolling();
  vi.useRealTimers();
});

describe("chat attachment sends", () => {
  it("adds a bubble, shows upload progress and sends the share contract", async () => {
    const pending = sendAttachmentMessage([draftFor(file())], "  Look at this  ", { lat: 45.4642, lon: 9.19 });
    const message = chat.messages[0]!;
    expect(message).toMatchObject({ role: "user", text: "Look at this", channel: "web", status: "sending" });
    expect(message.share).toMatchObject({ progress: 0, phase: "uploading" });
    expect(message.attachments?.map((a) => a.kind)).toEqual(["image", "location"]);

    const xhr = FakeXHR.instances[0]!;
    xhr.progress(40, 100);
    expect(message.share?.progress).toBeCloseTo(0.4);
    xhr.progress(10, 100);
    expect(message.share?.progress).toBeCloseTo(0.4);

    const form = xhr.body!;
    expect(Object.fromEntries([...form.entries()].filter(([key]) => key !== "file"))).toMatchObject({
      intent: "ask",
      lang: "en",
      source: "web-chat",
      channel: "web",
      note: "Look at this",
      shareId: message.share?.id,
    });
    expect(form.get("sessionId")).toBeTruthy();
    expect(JSON.parse(String(form.get("meta")))).toEqual({
      files: [{ index: 0 }],
      location: { lat: 45.4642, lon: 9.19 },
    });
    expect(form.getAll("file").map((f) => (f as File).name)).toEqual(["photo.png"]);

    xhr.finish(202, { stage: "received", userItemId: "u1" });
    await pending;
    const sent = chat.messages[0]!;
    expect(sent.status).toBe("sent");
    expect(sent.itemId).toBe("u1");
    expect(sent.share).toMatchObject({ progress: 1, phase: "received", stage: "received" });
    expect(sent.error).toBeUndefined();
  });

  it("retries with the same shareId after a failure", async () => {
    const pending = sendAttachmentMessage([draftFor(file())], "retry me");
    const key = chat.messages[0]!.key;
    const first = FakeXHR.instances[0]!;
    const id = first.body!.get("shareId");
    first.finish(500, { error: "Unavailable" });
    await pending;
    expect(chat.messages[0]).toMatchObject({ status: "error", error: "Not sent — Retry" });
    expect(chat.messages[0]!.share).toMatchObject({ phase: "error", error: "Unavailable" });
    // The files cannot survive a reload, so the unsent bubble is not cached.
    expect(sessionStorage.getItem("secretary.timeline.v1")).toBe("[]");

    const retry = retryMessage(key);
    const second = FakeXHR.instances[1]!;
    expect(second.body!.get("shareId")).toBe(id);
    expect(chat.messages[0]).toMatchObject({ status: "sending" });
    second.finish(202, { stage: "reading", userItemId: "u2" });
    await retry;
    expect(chat.messages[0]).toMatchObject({ status: "sent", itemId: "u2" });
    expect(chat.messages[0]!.share?.phase).toBe("reading");

    // Once accepted it is cached as a plain message (no share state, no blob previews).
    reloadChatFromStorage();
    expect(chat.messages[0]).toMatchObject({ text: "retry me", status: "sent", itemId: "u2" });
    expect(chat.messages[0]!.share).toBeUndefined();
    expect(chat.messages[0]!.attachments?.[0]?.previewUrl).toBeUndefined();
  });

  it("applies status polling and marks a server error as retryable", async () => {
    vi.useFakeTimers();
    let stage = "reading";
    const calls = stubFetch((call) => {
      if (!call.url.startsWith("/v1/share/")) return { body: {} };
      return stage === "reading"
        ? { body: { stage: "reading", attachments: [{ index: 0, kind: "image", mediaId: "m1", state: "read", summary: "A photo" }] } }
        : { body: { stage: "error", error: "Could not read this file" } };
    });

    const pending = sendAttachmentMessage([draftFor(file())]);
    FakeXHR.instances[0]!.finish(202, { stage: "received" });
    await pending;
    const message = chat.messages[0]!;
    expect(message.share?.phase).toBe("received");

    await vi.advanceTimersByTimeAsync(2_000);
    expect(message.share?.phase).toBe("reading");
    expect(message.share?.attachments).toEqual([{ index: 0, kind: "image", mediaId: "m1", state: "read", summary: "A photo" }]);
    expect(message.attachments?.[0]?.mediaId).toBe("m1");
    expect(message.attachments?.[0]?.name).toBe("photo.png");

    stage = "error";
    await vi.advanceTimersByTimeAsync(2_000);
    expect(message.share).toMatchObject({ phase: "error", error: "Could not read this file" });
    expect(message).toMatchObject({ status: "error", error: "Could not read this file" });
    const fetched = calls.filter((c) => c.url.startsWith("/v1/share/")).length;
    expect(fetched).toBe(2);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(calls.filter((c) => c.url.startsWith("/v1/share/")).length).toBe(2);
  });
});
