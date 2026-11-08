import { afterEach, describe, expect, it, vi } from "vitest";
import { chat, chatDetail, detail, message, notebook, podcast, source, sse, stubNotebooks } from "../test/notebooksFixtures";
import { ApiError } from "./client";
import { chunkOrdinal, hasPending, notebooksApi, parseSse, profiles, refSourceId, sendMessage } from "./notebooks";

afterEach(() => vi.unstubAllGlobals());

describe("requests", () => {
  it("lists notebooks, with the archived filter only when it is not the default", async () => {
    const calls = stubNotebooks(() => ({ body: { notebooks: [notebook()] } }));
    expect(await notebooksApi.list()).toHaveLength(1);
    await notebooksApi.list("only");
    expect(calls[0]).toMatchObject({ method: "GET", url: "/v1/notebooks" });
    expect(calls[1]).toMatchObject({ method: "GET", url: "/v1/notebooks?archived=only" });
  });

  it("creates, renames, archives and deletes (with what to do with the sources)", async () => {
    const calls = stubNotebooks(() => ({ body: notebook() }));
    await notebooksApi.create({ name: "CNS", description: "" });
    await notebooksApi.update("nb1", { archived: true });
    await notebooksApi.remove("nb1", "delete-exclusive");
    expect(calls[0]).toMatchObject({ method: "POST", url: "/v1/notebooks", body: { name: "CNS", description: "" } });
    expect(calls[0]!.headers["x-secretary-csrf"]).toBe("1");
    expect(calls[1]).toMatchObject({ method: "PATCH", url: "/v1/notebooks/nb1", body: { archived: true } });
    expect(calls[2]).toMatchObject({ method: "DELETE", url: "/v1/notebooks/nb1?sources=delete-exclusive" });
  });

  it("adds each kind of source, and moves its context", async () => {
    const calls = stubNotebooks(() => ({ status: 202, body: source() }));
    await notebooksApi.addSource("nb1", { kind: "url", url: "https://youtu.be/x" });
    await notebooksApi.addSource("nb1", { kind: "text", title: "Memo", text: "hello" });
    await notebooksApi.addSource("nb1", { kind: "sandbox", path: "/home/cua/a.pdf" });
    await notebooksApi.addSource("nb1", { kind: "note", slug: "my-note" });
    await notebooksApi.setContext("nb1", "s1", "summary");
    expect(calls.map((c) => c.body)).toEqual([
      { kind: "url", url: "https://youtu.be/x" },
      { kind: "text", title: "Memo", text: "hello" },
      { kind: "sandbox", path: "/home/cua/a.pdf" },
      { kind: "note", slug: "my-note" },
      { context: "summary" },
    ]);
    expect(calls[4]).toMatchObject({ method: "PATCH", url: "/v1/notebooks/nb1/sources/s1" });
  });

  it("uploads a file as multipart under the field name `file`", async () => {
    const calls = stubNotebooks(() => ({ status: 202, body: source() }));
    await notebooksApi.uploadSource("nb1", new File(["%PDF-1.4"], "RM Class 2.pdf", { type: "application/pdf" }), "Class 2");
    const c = calls[0]!;
    expect(c).toMatchObject({ method: "POST", url: "/v1/notebooks/nb1/sources/upload" });
    expect(c.headers["content-type"]).toMatch(/^multipart\/form-data; boundary=/);
    const text = await c.form!.text();
    expect(text).toContain('name="file"; filename="RM Class 2.pdf"');
    expect(text).toContain('name="title"');
    expect(text).toContain("%PDF-1.4");
  });

  it("reads passages around a citation, or a window from the start", async () => {
    const calls = stubNotebooks(() => ({ body: { chunks: [{ id: "s1-3", ordinal: 3, text: "x" }] } }));
    expect(await notebooksApi.chunks("s1", { around: 3 })).toHaveLength(1);
    await notebooksApi.chunks("s1", { from: 0, limit: 40 });
    await notebooksApi.chunks("s1");
    expect(calls.map((c) => c.url)).toEqual([
      "/v1/notebooks/sources/s1/chunks?around=3",
      "/v1/notebooks/sources/s1/chunks?from=0&limit=40",
      "/v1/notebooks/sources/s1/chunks",
    ]);
  });

  it("runs transformations, turns insights and replies into notes, writes and unlinks notes", async () => {
    const calls = stubNotebooks(() => ({ status: 201, body: { slug: "n" } }));
    await notebooksApi.runTransformation("s1", "t-dense-summary");
    await notebooksApi.insightToNote("i1");
    await notebooksApi.messageToNote("c1", "m1", "Answer");
    await notebooksApi.writeNote("nb1", "Title", "Body");
    await notebooksApi.linkNote("nb1", "a-note");
    await notebooksApi.unlinkNote("nb1", "a-note");
    expect(calls.map((c) => [c.method, c.url, c.body])).toEqual([
      ["POST", "/v1/notebooks/sources/s1/insights", { transformation_id: "t-dense-summary" }],
      ["POST", "/v1/notebooks/insights/i1/note", {}],
      ["POST", "/v1/notebooks/chats/c1/messages/m1/note", { title: "Answer" }],
      ["POST", "/v1/notebooks/nb1/notes", { title: "Title", text: "Body" }],
      ["POST", "/v1/notebooks/nb1/notes", { slug: "a-note" }],
      ["DELETE", "/v1/notebooks/nb1/notes/a-note", undefined],
    ]);
  });

  it("searches, manages chats and podcasts at the contract's routes", async () => {
    const calls = stubNotebooks((c) => (c.path.endsWith("/search") ? { body: { hits: [] } } : { body: chat() }));
    await notebooksApi.search("nb1", "valid ity", 5);
    await notebooksApi.createChat("nb1", "Exam");
    await notebooksApi.renameChat("c1", "Final");
    await notebooksApi.removeChat("c1");
    await notebooksApi.createPodcast("nb1", { episode_profile: "two-hosts", minutes: 8 });
    await notebooksApi.removePodcast("p1");
    expect(calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET /v1/notebooks/nb1/search?q=valid+ity&limit=5",
      "POST /v1/notebooks/nb1/chats",
      "PATCH /v1/notebooks/chats/c1",
      "DELETE /v1/notebooks/chats/c1",
      "POST /v1/notebooks/nb1/podcasts",
      "DELETE /v1/notebooks/podcasts/p1",
    ]);
    expect(calls[1]!.body).toEqual({ title: "Exam" });
    expect(notebooksApi.audioUrl("p1")).toBe("/v1/notebooks/podcasts/p1/audio");
    expect(notebooksApi.fileUrl("s 1")).toBe("/v1/notebooks/sources/s%201/file");
  });

  it("turns the server's error envelope into an ApiError", async () => {
    stubNotebooks(() => ({ status: 409, body: { error: { code: "conflict", message: "Built-in transformations can't be deleted." } } }));
    const err = await notebooksApi.get("nb1").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(err).toMatchObject({ status: 409, code: "conflict", message: "Built-in transformations can't be deleted." });
  });

  it("reads podcast profiles given as names or as objects", async () => {
    stubNotebooks(() => ({
      body: { episode_profiles: ["two-hosts", { name: "Deep dive", description: "One host" }], speaker_profiles: [{ id: "lucina-duo", name: "Lucina and Marco" }, 7] },
    }));
    const p = await notebooksApi.podcastProfiles();
    expect(p.episode_profiles).toEqual([
      { id: "two-hosts", name: "two-hosts" },
      { id: "Deep dive", name: "Deep dive", description: "One host" },
    ]);
    expect(p.speaker_profiles).toEqual([{ id: "lucina-duo", name: "Lucina and Marco" }]);
    expect(profiles(undefined)).toEqual([]);
  });
});

describe("helpers", () => {
  it("reads the passage out of a citation ref", () => {
    expect(chunkOrdinal("source:s0a9be3c112-14")).toBe(14);
    expect(refSourceId("source:s0a9be3c112-14")).toBe("s0a9be3c112");
    expect(refSourceId("insight:i1")).toBeNull();
    expect(chunkOrdinal("note:abc")).toBeNull();
  });

  it("knows when a notebook still has work in progress", () => {
    const base = detail();
    expect(hasPending(base)).toBe(false);
    expect(hasPending({ ...base, sources: [source({ status: "embedding" })] })).toBe(true);
    expect(hasPending({ ...base, sources: [source({ status: "failed" })] })).toBe(false);
    expect(hasPending({ ...base, podcasts: [podcast({ status: "voicing" })] })).toBe(true);
    expect(hasPending({ ...base, podcasts: [podcast({ status: "ready" })] })).toBe(false);
  });
});

describe("server-sent events", () => {
  it("splits complete events and keeps the unfinished one", () => {
    const first = parseSse('event: delta\ndata: {"text":"Hel"}\n\nevent: delta\ndata: {"te');
    expect(first.events).toEqual([{ event: "delta", data: '{"text":"Hel"}' }]);
    const second = parseSse(`${first.rest}xt":"lo"}\n\n`);
    expect(second.events).toEqual([{ event: "delta", data: '{"text":"lo"}' }]);
    expect(second.rest).toBe("");
  });

  it("handles CRLF, comments and multi-line data", () => {
    const { events } = parseSse(": ping\r\n\r\nevent: message\r\ndata: a\r\ndata: b\r\n\r\n");
    expect(events).toEqual([{ event: "message", data: "a\nb" }]);
  });

  it("streams a reply: deltas as they come, then the stored exchange", async () => {
    const exchange = { user: message({ id: "u", role: "user", text: "Q" }), reply: message({ id: "r", text: "Hello there" }) };
    const calls = stubNotebooks(() => ({ sse: [sse("delta", { text: "Hello" }), 'event: delta\ndata: {"text":" th', 'ere"}\n\n', sse("message", exchange)] }));
    const pieces: string[] = [];
    const out = await sendMessage("c1", { text: "Q", model: "qwen" }, (p) => pieces.push(p));
    expect(pieces).toEqual(["Hello", " there"]);
    expect(out).toEqual(exchange);
    expect(calls[0]).toMatchObject({ method: "POST", url: "/v1/notebooks/chats/c1/messages", body: { text: "Q", model: "qwen" } });
    expect(calls[0]!.headers.accept).toContain("text/event-stream");
    expect(calls[0]!.headers["x-secretary-csrf"]).toBe("1");
  });

  it("falls back to plain JSON when the server does not stream", async () => {
    const exchange = { user: message({ role: "user" }), reply: message({ id: "r" }) };
    stubNotebooks(() => ({ body: exchange }));
    const pieces: string[] = [];
    expect(await notebooksApi.sendMessage("c1", { text: "Q" }, (p) => pieces.push(p))).toEqual(exchange);
    expect(pieces).toEqual([]);
  });

  it("reports an error event, a failed status and a stream that ends early", async () => {
    stubNotebooks(() => ({ sse: [sse("error", { error: { code: "unavailable", message: "The model is busy." } })] }));
    await expect(sendMessage("c1", { text: "Q" })).rejects.toMatchObject({ code: "unavailable", message: "The model is busy." });
    stubNotebooks(() => ({ status: 404, body: { error: { code: "not_found", message: "No such chat." } } }));
    await expect(sendMessage("c1", { text: "Q" })).rejects.toMatchObject({ status: 404, message: "No such chat." });
    stubNotebooks(() => ({ sse: [sse("delta", { text: "partial" })] }));
    await expect(sendMessage("c1", { text: "Q" })).rejects.toMatchObject({ code: "stream" });
    vi.stubGlobal("fetch", vi.fn(async () => Promise.reject(new TypeError("offline"))));
    await expect(sendMessage("c1", { text: "Q" })).rejects.toMatchObject({ code: "network" });
  });

  it("reads the chat with its messages", async () => {
    stubNotebooks(() => ({ body: chatDetail() }));
    const c = await notebooksApi.chat("c1");
    expect(c.messages[1]!.citations?.[0]?.ref).toBe("source:s1-3");
  });
});
