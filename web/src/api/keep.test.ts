import { afterEach, describe, expect, it, vi } from "vitest";
import { stubFetch } from "../test/fetch";
import { ApiError } from "./client";
import { attachmentUrl, cleanLabelName, importGoogleKeep, keepApi, newItemId, normLabel, parseChanges, parseImportReport, parseKeepNote } from "./keep";

describe("parsing the server's notes", () => {
  it("fills the defaults an old note file lacks", () => {
    const n = parseKeepNote({ id: "n_1", slug: "old", title: "Old", content: "x", tags: ["a"], pinned: true, createdAt: "2026-10-20T10:00:00.000Z", updatedAt: "2026-10-20T10:00:00.000Z" });
    expect(n).toMatchObject({ color: "default", archived: false, attachments: [], annotations: [], collaborators: [], rev: 0, pinned: true, source: "owner" });
    expect(n.items).toBeUndefined();
    expect(n.position).toBe(Date.parse("2026-10-20T10:00:00.000Z"));
  });

  it("keeps a list note's items (even an empty list) and its reminder", () => {
    const n = parseKeepNote({ id: "n_1", items: [{ id: "i_1", text: "a", checked: true, indent: 1 }, { text: "b" }], reminder: { at: "2026-10-30T09:00:00Z", recurrence: "weekly", lucinaId: "r_1" } });
    expect(n.items).toHaveLength(2);
    expect(n.items?.[0]).toEqual({ id: "i_1", text: "a", checked: true, indent: 1 });
    expect(n.items?.[1]).toMatchObject({ text: "b", checked: false, indent: 0 });
    expect(n.reminder).toEqual({ at: "2026-10-30T09:00:00Z", recurrence: "weekly", lucinaId: "r_1" });
    expect(parseKeepNote({ id: "n_2", items: [] }).items).toEqual([]);
  });

  it("falls back to the default colour for one it does not know, and ignores a bad recurrence", () => {
    expect(parseKeepNote({ id: "n", color: "magenta" }).color).toBe("default");
    expect(parseKeepNote({ id: "n", reminder: { at: "2026-10-30T09:00:00Z", recurrence: "hourly" } }).reminder).toEqual({ at: "2026-10-30T09:00:00Z" });
  });

  it("reads the changes feed, with a numeric cursor too", () => {
    const c = parseChanges({ notes: [{ id: "a", rev: 3 }, { nope: 1 }], removed: ["z", 5], cursor: 7, hasMore: true });
    expect(c.notes.map((n) => n.id)).toEqual(["a"]);
    expect(c.removed).toEqual(["z"]);
    expect(c).toMatchObject({ cursor: "7", hasMore: true });
  });

  it("normalises labels like the server", () => {
    expect(normLabel("  #Fisica   2 ")).toBe("fisica 2");
    expect(cleanLabelName("  #Fisica   2 ")).toBe("Fisica 2");
    expect(newItemId()).toMatch(/^i_[0-9a-f]{12}$/);
    expect(attachmentUrl({ file: "ab.png" })).toBe("/v1/notes/attachments/ab.png");
  });
});

describe("requests", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("asks for changes since the cursor", async () => {
    const calls = stubFetch(() => ({ body: { notes: [], removed: [], cursor: "5", hasMore: false } }));
    await keepApi.changes("4", 100);
    expect(calls[0]).toMatchObject({ method: "GET", url: "/v1/notes/changes?since=4&limit=100" });
    expect(calls[0]!.credentials).toBe("same-origin");
  });

  it("sends writes with the CSRF header", async () => {
    const calls = stubFetch(() => ({ body: { id: "n_1" } }));
    await keepApi.patch("n_1", { color: "teal", labels: ["GodLevel"] });
    expect(calls[0]).toMatchObject({ method: "PATCH", url: "/v1/notes/n_1", body: { color: "teal", labels: ["GodLevel"] } });
    expect(calls[0]!.headers["x-secretary-csrf"]).toBe("1");
  });

  it("uses the batch, label and trash endpoints of the contract", async () => {
    const calls = stubFetch(() => ({ body: { notes: [], labels: [], ok: true } }));
    await keepApi.batch({ ids: ["a"], set: { archived: true } });
    await keepApi.deleteForever("a");
    await keepApi.emptyTrash();
    await keepApi.renameLabel("fisica 2", "Fisica Due");
    await keepApi.deleteLabel("chill");
    await keepApi.restore("a");
    expect(calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "POST /v1/notes/batch",
      "DELETE /v1/notes/trash/a",
      "DELETE /v1/notes/trash",
      "PATCH /v1/notes/labels/fisica%202",
      "DELETE /v1/notes/labels/chill",
      "POST /v1/notes/trash/a/restore",
    ]);
  });

  it("uploads an image as the raw body with its type", async () => {
    let seen: { type: string | null; body: unknown } | undefined;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_url: string, init: RequestInit) => {
        const h = init.headers as Record<string, string>;
        seen = { type: h["Content-Type"] ?? null, body: init.body };
        return new Response(JSON.stringify({ id: "n_1" }), { status: 201, headers: { "Content-Type": "application/json" } });
      }),
    );
    const blob = new Blob(["png-bytes"], { type: "image/png" });
    await keepApi.uploadAttachment("n_1", blob, "a b.png");
    expect(seen?.type).toBe("image/png");
    expect(seen?.body).toBe(blob);
    expect((fetch as unknown as ReturnType<typeof vi.fn>).mock.calls[0]![0]).toBe("/v1/notes/n_1/attachments?name=a%20b.png");
  });

  it("turns a 410 into an ApiError the store can read", async () => {
    stubFetch(() => ({ status: 410, body: { error: { code: "cursor_expired", message: "too old" } } }));
    await expect(keepApi.changes("1")).rejects.toMatchObject({ status: 410, code: "cursor_expired" });
  });
});

class FakeXhr {
  static last: FakeXhr;
  method = "";
  url = "";
  headers: Record<string, string> = {};
  sent: unknown;
  withCredentials = false;
  responseType = "";
  timeout = 0;
  status = 200;
  responseText = "";
  upload: { onprogress: ((e: { lengthComputable: boolean; loaded: number; total: number }) => void) | null } = { onprogress: null };
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  ontimeout: (() => void) | null = null;
  onabort: (() => void) | null = null;
  constructor() {
    FakeXhr.last = this;
  }
  open(m: string, u: string) {
    this.method = m;
    this.url = u;
  }
  setRequestHeader(k: string, v: string) {
    this.headers[k] = v;
  }
  send(body: unknown) {
    this.sent = body;
  }
  abort() {
    this.onabort?.();
  }
}

describe("the Takeout upload", () => {
  afterEach(() => vi.unstubAllGlobals());

  const report = { dryRun: true, files: { json: 2, html: 2, attachments: 1, ignored: 0 }, notes: { total: 2, new: 2, updated: 0, unchanged: 0, keptLocalEdits: 0, skippedDeleted: 0, trashed: 0, archived: 1, pinned: 0, lists: 1 }, labels: { total: 1, new: ["Chill"] }, attachments: { total: 1, new: 1, missing: [], bytes: 1234 }, errors: [], sample: [{ title: "A", labels: ["Chill"], color: "teal", kind: "list" }] };

  it("posts the zip raw, with ?dryRun=1 for a check, and reports the progress", async () => {
    vi.stubGlobal("XMLHttpRequest", FakeXhr);
    const progress: number[] = [];
    const file = new Blob(["zip"], { type: "application/zip" });
    const p = importGoogleKeep(file, { dryRun: true, onProgress: (f) => progress.push(f) });
    const x = FakeXhr.last;
    expect(x.method).toBe("POST");
    expect(x.url).toBe("/v1/notes/import/google-keep?dryRun=1");
    expect(x.headers["X-Secretary-CSRF"]).toBe("1");
    expect(x.headers["Content-Type"]).toBe("application/zip");
    expect(x.withCredentials).toBe(true);
    expect(x.sent).toBe(file);
    x.upload.onprogress?.({ lengthComputable: true, loaded: 50, total: 200 });
    x.upload.onprogress?.({ lengthComputable: true, loaded: 200, total: 200 });
    x.responseText = JSON.stringify(report);
    x.onload?.();
    const r = await p;
    expect(progress).toEqual([0.25, 1, 1]);
    expect(r.notes.total).toBe(2);
    expect(r.labels.new).toEqual(["Chill"]);
  });

  it("posts to the plain endpoint to import", async () => {
    vi.stubGlobal("XMLHttpRequest", FakeXhr);
    const p = importGoogleKeep(new Blob(["z"]), { dryRun: false });
    expect(FakeXhr.last.url).toBe("/v1/notes/import/google-keep");
    FakeXhr.last.responseText = JSON.stringify({ ...report, dryRun: false });
    FakeXhr.last.onload?.();
    expect((await p).dryRun).toBe(false);
  });

  it("rejects with the server's error code, e.g. an import already running", async () => {
    vi.stubGlobal("XMLHttpRequest", FakeXhr);
    const p = importGoogleKeep(new Blob(["z"]), { dryRun: false });
    FakeXhr.last.status = 409;
    FakeXhr.last.responseText = JSON.stringify({ error: { code: "import_running", message: "another import is running" } });
    FakeXhr.last.onload?.();
    await expect(p).rejects.toMatchObject({ status: 409, code: "import_running" });
    await expect(p).rejects.toBeInstanceOf(ApiError);
  });

  it("rejects on a network error", async () => {
    vi.stubGlobal("XMLHttpRequest", FakeXhr);
    const p = importGoogleKeep(new Blob(["z"]), { dryRun: true });
    FakeXhr.last.onerror?.();
    await expect(p).rejects.toMatchObject({ code: "network" });
  });

  it("fills a partial report with zeros", () => {
    const r = parseImportReport({ notes: { total: 3 } }, true);
    expect(r.dryRun).toBe(true);
    expect(r.notes).toMatchObject({ total: 3, new: 0, lists: 0 });
    expect(r.errors).toEqual([]);
  });
});
