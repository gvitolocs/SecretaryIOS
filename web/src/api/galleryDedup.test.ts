import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "./client";
import {
  createStack,
  deleteStack,
  dismissDuplicates,
  dismissStackSuggestion,
  getStack,
  listDuplicates,
  listStackSuggestions,
  parseDuplicateGroup,
  parseItem,
  resolveAllDuplicates,
  resolveDuplicates,
  setStackPrimary,
  undoDuplicates,
} from "./gallery";
import { stubFetch } from "../test/fetch";
import { rawItem } from "../test/galleryGrid";

afterEach(() => vi.unstubAllGlobals());

const group = (extra: Record<string, unknown> = {}) => ({
  id: "dg_1",
  class: "same_photo_reencoded",
  score: 0.93,
  applicable: true,
  keep: "a",
  reason: "largest_file",
  members: [
    { ...rawItem("a"), review: { metadata: 5, keep: true } },
    { ...rawItem("b"), review: { metadata: 2, keep: false } },
  ],
  ...extra,
});

describe("the stack on an item", () => {
  it("reads the stack reference, and a single photo has none", () => {
    expect(parseItem(rawItem("a", { stack: { id: "stk_1", count: 4, primary: true } }))?.stack).toEqual({ id: "stk_1", count: 4, primary: true });
    expect(parseItem(rawItem("a", { stack: { id: "stk_1", count: 4, primary: false } }))?.stack?.primary).toBe(false);
    expect(parseItem(rawItem("a"))?.stack).toBeNull();
    expect(parseItem(rawItem("a", { stack: { id: "stk_1", count: 1, primary: true } }))?.stack).toBeNull();
    expect(parseItem(rawItem("a", { stack: "nope" }))?.stack).toBeNull();
  });
});

describe("duplicate groups", () => {
  it("parses a group with its members, the metadata count and the keeper", () => {
    const g = parseDuplicateGroup(group());
    expect(g).toMatchObject({ id: "dg_1", class: "same_photo_reencoded", score: 0.93, applicable: true, keep: "a", reason: "largest_file" });
    expect(g?.members.map((m) => [m.item.id, m.metadata, m.keep])).toEqual([
      ["a", 5, true],
      ["b", 2, false],
    ]);
  });

  it("drops malformed groups: an unknown class, no keeper, or fewer than two readable members", () => {
    expect(parseDuplicateGroup(group({ class: "burst_neighbor" }))).toBeNull();
    expect(parseDuplicateGroup(group({ keep: "" }))).toBeNull();
    expect(parseDuplicateGroup(group({ members: [{ ...rawItem("a"), review: {} }, { nope: true }] }))).toBeNull();
    expect(parseDuplicateGroup(null)).toBeNull();
  });

  it("falls back to a known reason and clamps the score", () => {
    const g = parseDuplicateGroup(group({ reason: "mystery", score: 3 }));
    expect(g?.reason).toBe("largest_file");
    expect(g?.score).toBe(1);
  });

  it("lists the open groups with the counts of the page", async () => {
    const calls = stubFetch(() => ({
      body: {
        ok: true,
        hashes: { photos: 100, hashed: 90, pending: 10 },
        total: 7,
        applicable: 5,
        min_score: 0.7,
        reclaimable_bytes: 12345,
        stack_suggestions: 2,
        groups: [group(), { junk: 1 }],
      },
    }));
    const page = await listDuplicates({ limit: 20, offset: 40 });
    expect(calls[0]!.url).toBe("/v1/media/duplicates?status=open&limit=20&offset=40");
    expect(page).toMatchObject({ total: 7, applicable: 5, minScore: 0.7, reclaimableBytes: 12345, stackSuggestions: 2 });
    expect(page.hashes).toEqual({ photos: 100, hashed: 90, pending: 10 });
    expect(page.groups).toHaveLength(1);
  });

  it("resolves: the keeper first, the copies as drop", async () => {
    const calls = stubFetch(() => ({ body: { ok: true, merged: [{ id: "b", into: "a" }], skipped: [{ id: "c", reason: "stacked" }] } }));
    const result = await resolveDuplicates(["a"], ["b", "c"]);
    expect(calls[0]).toMatchObject({ method: "POST", url: "/v1/media/duplicates/resolve", body: { keep: ["a"], drop: ["b", "c"] } });
    expect(result.merged).toEqual([{ id: "b", into: "a" }]);
    expect(result.skipped).toEqual([{ id: "c", reason: "stacked" }]);
  });

  it("dismisses, takes a dismissal back, and undoes a merge", async () => {
    const calls = stubFetch((call) => ({ body: call.url.endsWith("/undo") ? { ok: true, undone: ["b"], not_merged: ["c"] } : { ok: true, pairs: 1 } }));
    await dismissDuplicates(["a", "b"]);
    await dismissDuplicates(["a", "b"], true);
    const undone = await undoDuplicates(["b", "c"]);
    expect(calls.map((c) => c.body)).toEqual([{ ids: ["a", "b"] }, { ids: ["a", "b"], undo: true }, { ids: ["b", "c"] }]);
    expect(undone).toEqual({ undone: ["b"], notMerged: ["c"] });
  });

  it("applies all suggestions, optionally with a higher score", async () => {
    const calls = stubFetch(() => ({ body: { ok: true, groups: 2, merged: 3, merged_ids: [{ id: "b", into: "a" }, { id: "d", into: "c" }, { id: "e", into: "c" }], skipped: { stacked: 1 } } }));
    const result = await resolveAllDuplicates();
    await resolveAllDuplicates(0.9);
    expect(calls.map((c) => c.body)).toEqual([{}, { min_score: 0.9 }]);
    expect(result.groups).toBe(2);
    expect(result.merged).toHaveLength(3);
    expect(result.skipped).toEqual({ stacked: 1 });
  });
});

const stackBody = {
  ok: true,
  stack: { id: "stk_1", kind: "burst", primary: "a", count: 2, created_at: "2026-10-28T10:00:00Z", items: [rawItem("a"), rawItem("b")] },
};

describe("stacks", () => {
  it("reads a stack with its members", async () => {
    const calls = stubFetch(() => ({ body: stackBody }));
    const stack = await getStack("stk_1");
    expect(calls[0]!.url).toBe("/v1/media/stacks/stk_1");
    expect(stack).toMatchObject({ id: "stk_1", kind: "burst", primary: "a", count: 2 });
    expect(stack.items.map((i) => i.id)).toEqual(["a", "b"]);
  });

  it("creates, changes the primary and unstacks", async () => {
    const calls = stubFetch((call) => (call.method === "DELETE" ? { body: { ok: true, unstacked: ["a", "b"] } } : { status: call.method === "POST" ? 201 : 200, body: stackBody }));
    await createStack(["a", "b"], { primary: "b", kind: "manual" });
    await setStackPrimary("stk_1", "b");
    const gone = await deleteStack("stk_1");
    expect(calls.map((c) => [c.method, c.url, c.body])).toEqual([
      ["POST", "/v1/media/stacks", { ids: ["a", "b"], primary: "b", kind: "manual" }],
      ["PUT", "/v1/media/stacks/stk_1", { primary: "b" }],
      ["DELETE", "/v1/media/stacks/stk_1", undefined],
    ]);
    expect(gone).toEqual(["a", "b"]);
  });

  it("reports a refused stack (409) as an ApiError with the status", async () => {
    stubFetch(() => ({ status: 409, body: { ok: false, error: "a: already_stacked", failed: [{ id: "a", reason: "already_stacked" }] } }));
    const err = await createStack(["a", "b"]).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(409);
  });

  it("an answer without a stack is an error", async () => {
    stubFetch(() => ({ body: { ok: true } }));
    await expect(getStack("stk_1")).rejects.toThrow();
  });

  it("lists suggestions, skipping malformed ones, and dismisses a set", async () => {
    const calls = stubFetch((call) =>
      call.method === "GET"
        ? {
            body: {
              ok: true,
              hashes_pending: 3,
              total: 2,
              suggestions: [
                { id: "sg_1", kind: "burst", evidence: "time", primary: "a", span_ms: 900, items: [rawItem("a"), rawItem("b")] },
                { id: "sg_2", kind: "burst", primary: "a", items: [rawItem("a")] },
              ],
            },
          }
        : { body: { ok: true } },
    );
    const result = await listStackSuggestions(10);
    await dismissStackSuggestion(["a", "b"]);
    expect(calls[0]!.url).toBe("/v1/media/stacks/suggestions?limit=10");
    expect(result.hashesPending).toBe(3);
    expect(result.suggestions).toHaveLength(1);
    expect(result.suggestions[0]).toMatchObject({ id: "sg_1", kind: "burst", evidence: "time", primary: "a", spanMs: 900 });
    expect(calls[1]).toMatchObject({ method: "POST", url: "/v1/media/stacks/suggestions/dismiss", body: { ids: ["a", "b"] } });
  });
});
