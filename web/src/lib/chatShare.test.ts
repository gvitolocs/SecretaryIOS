import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { stubFetch } from "../test/fetch";
import {
  buildChatShareForm,
  parseShareAccepted,
  parseShareStatus,
  pollShareStatus,
  postChatShare,
  ShareUploadError,
  type ShareUploadHandlers,
} from "./chatShare";
import type { DraftAttachment } from "./attachments";

class FakeXHR {
  static instances: FakeXHR[] = [];
  upload: { onprogress?: (event: ProgressEvent) => void } = {};
  status = 0;
  responseText = "";
  withCredentials = false;
  timeout = 0;
  body: FormData | null = null;
  headers: Record<string, string> = {};
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  ontimeout: (() => void) | null = null;
  onabort: (() => void) | null = null;
  open = vi.fn();
  setRequestHeader = vi.fn((name: string, value: string) => {
    this.headers[name] = value;
  });
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
    this.responseText = typeof body === "string" ? body : JSON.stringify(body);
    this.onload?.();
  }
}

const draft = (name = "photo.png") =>
  ({ id: "d1", file: new File(["x"], name, { type: "image/png" }), kind: "image", name, size: 1, mime: "image/png" }) as DraftAttachment;

beforeEach(() => {
  FakeXHR.instances = [];
  vi.stubGlobal("XMLHttpRequest", FakeXHR as unknown as typeof XMLHttpRequest);
});

afterEach(() => {
  vi.useRealTimers();
});

describe("buildChatShareForm", () => {
  it("sends the web-chat contract fields, note, meta and files", () => {
    const form = buildChatShareForm({
      shareId: "s1",
      sessionId: "sess",
      caption: "Look at this",
      meta: '{"files":[{"index":0,"width":10}]}',
      files: [draft("a.png"), draft("b.png")],
    });
    expect(Object.fromEntries([...form.entries()].filter(([key]) => key !== "file"))).toEqual({
      shareId: "s1",
      intent: "ask",
      lang: "en",
      source: "web-chat",
      channel: "web",
      sessionId: "sess",
      note: "Look at this",
      meta: '{"files":[{"index":0,"width":10}]}',
    });
    expect(form.getAll("file").map((f) => (f as File).name)).toEqual(["a.png", "b.png"]);
  });

  it("omits meta when the caller has nothing to add", () => {
    const form = buildChatShareForm({ shareId: "s1", sessionId: "s", caption: "", files: [draft()], lang: "it" });
    expect(form.has("meta")).toBe(false);
    expect(form.get("lang")).toBe("it");
    expect(form.get("note")).toBe("");
  });
});

describe("postChatShare", () => {
  it("reports upload progress and resolves the 202 JSON", async () => {
    const progress = vi.fn();
    const pending = postChatShare(
      { shareId: "s1", sessionId: "s", caption: "hi", files: [draft()] },
      { onProgress: progress } satisfies ShareUploadHandlers,
    );
    const xhr = FakeXHR.instances[0]!;
    expect(xhr.open).toHaveBeenCalledWith("POST", "/v1/share");
    expect(xhr.withCredentials).toBe(true);
    expect(xhr.headers["X-Secretary-CSRF"]).toBe("1");
    xhr.progress(25, 100);
    expect(progress).toHaveBeenCalledWith(25, 100);
    xhr.progress(120, 100);
    expect(progress).toHaveBeenLastCalledWith(100, 100);
    xhr.finish(202, { stage: "received", userItemId: "u1", lucinaItemId: 7 });
    await expect(pending).resolves.toMatchObject({ stage: "received", userItemId: "u1", lucinaItemId: "7" });
  });

  it("rejects with the server's error message", async () => {
    const pending = postChatShare({ shareId: "s1", sessionId: "s", caption: "", files: [draft()] });
    FakeXHR.instances[0]!.finish(500, { error: "No storage" });
    await expect(pending).rejects.toMatchObject({ name: "ShareUploadError", message: "No storage", status: 500 });
  });

  it("rejects on network errors", async () => {
    const pending = postChatShare({ shareId: "s1", sessionId: "s", caption: "", files: [draft()] });
    FakeXHR.instances[0]!.onerror?.();
    await expect(pending).rejects.toBeInstanceOf(ShareUploadError);
  });
});

describe("parsing", () => {
  it("accepts a nested share object and coerces ids", () => {
    expect(parseShareAccepted({ share: { stage: "reading", userItemId: 12 } })).toMatchObject({
      stage: "reading",
      userItemId: "12",
    });
    expect(parseShareAccepted("nope")).toEqual({ raw: "nope" });
  });

  it("normalises status stages, attachments and errors", () => {
    const status = parseShareStatus({
      share: {
        stage: "reading",
        status: "thinking",
        userItemId: "u1",
        attachments: [
          { index: 0, kind: "image", mediaId: "m1", state: "read", summary: "A cat" },
          { index: -1, kind: "image" },
          { index: "2" },
          { index: 1, kind: "file", state: "weird" },
        ],
      },
    });
    expect(status.stage).toBe("reading");
    expect(status.userItemId).toBe("u1");
    expect(status.attachments).toEqual([
      { index: 0, kind: "image", mediaId: "m1", state: "read", summary: "A cat" },
      { index: 1, kind: "file", state: "pending" },
    ]);
    expect(parseShareStatus({ status: "error", error: "bad" })).toEqual({ stage: "error", attachments: [], error: "bad" });
    expect(parseShareStatus(null)).toEqual({ stage: "received", attachments: [] });
  });
});

describe("pollShareStatus", () => {
  it("polls every interval until the share is done", async () => {
    vi.useFakeTimers();
    let polled = 0;
    stubFetch(() => {
      polled++;
      return { body: { stage: polled === 1 ? "reading" : "done", attachments: [] } };
    });
    const updates: string[] = [];
    pollShareStatus("s1", {
      onUpdate: (status) => updates.push(status.stage),
      intervalMs: 1_000,
      visibility: () => "visible",
    });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(updates).toEqual(["reading"]);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(updates).toEqual(["reading", "done"]);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(polled).toBe(2);
  });

  it("stops immediately when the tab is already hidden", async () => {
    vi.useFakeTimers();
    const calls = stubFetch(() => ({ body: { stage: "reading" } }));
    pollShareStatus("s1", { onUpdate: () => {}, intervalMs: 1_000, visibility: () => "hidden" });
    await vi.advanceTimersByTimeAsync(5_000);
    expect(calls).toHaveLength(0);
  });

  it("stops when the tab becomes hidden", async () => {
    vi.useFakeTimers();
    let visible: DocumentVisibilityState = "visible";
    const calls = stubFetch(() => ({ body: { stage: "reading" } }));
    pollShareStatus("s1", { onUpdate: () => {}, intervalMs: 1_000, visibility: () => visible });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(calls).toHaveLength(1);
    visible = "hidden";
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.advanceTimersByTimeAsync(5_000);
    expect(calls).toHaveLength(1);
  });

  it("gives up after the maximum duration", async () => {
    vi.useFakeTimers();
    let now = 0;
    const calls = stubFetch(() => ({ body: { stage: "reading" } }));
    pollShareStatus("s1", {
      onUpdate: () => {},
      intervalMs: 1_000,
      maxMs: 2_500,
      visibility: () => "visible",
      now: () => now,
    });
    await vi.advanceTimersByTimeAsync(1_000);
    now = 2_000;
    await vi.advanceTimersByTimeAsync(1_000);
    now = 3_000;
    await vi.advanceTimersByTimeAsync(1_000);
    expect(calls).toHaveLength(2);
  });
});
