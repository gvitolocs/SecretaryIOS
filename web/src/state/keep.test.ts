import "fake-indexeddb/auto";
import { createRoot } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { ApiError } from "../api/client";
import type { Changes, KeepApi, KeepNote, LabelInfo, NoteInput } from "../api/keep";
import { createKeepDb } from "../lib/keepDb";
import { deferred } from "../test/fetch";
import { note } from "../test/keepFixtures";
import { applyInput, contentToItems, createKeepStore, itemsToContent, startAutoSync } from "./keep";

const changes = (notes: KeepNote[], cursor: string, extra: Partial<Changes> = {}): Changes => ({ notes, removed: [], cursor, hasMore: false, ...extra });

type Mocked = { [K in keyof KeepApi]: Mock<KeepApi[K]> };

function fakeApi(over: Partial<KeepApi> = {}): Mocked {
  const api = {
    changes: vi.fn(async () => changes([], "0")),
    labels: vi.fn(async (): Promise<LabelInfo[]> => []),
    create: vi.fn(async (i: NoteInput) => note({ id: "n_new", title: i.title ?? "", content: i.content ?? "", rev: 10 })),
    patch: vi.fn(async (id: string, i: NoteInput) => applyInput(note({ id, rev: 20 }), i, "2026-10-27T10:00:00.000Z")),
    trash: vi.fn(async () => undefined),
    restore: vi.fn(async (id: string) => note({ id })),
    deleteForever: vi.fn(async () => undefined),
    emptyTrash: vi.fn(async () => undefined),
    batch: vi.fn(async () => [] as KeepNote[]),
    createLabel: vi.fn(async (name: string) => ({ tag: name.toLowerCase(), name, count: 0 })),
    renameLabel: vi.fn(async (tag: string, name: string) => ({ tag, name, count: 0 })),
    deleteLabel: vi.fn(async () => undefined),
    uploadAttachment: vi.fn(async (id: string) => note({ id, rev: 30 })),
    deleteAttachment: vi.fn(async (id: string) => note({ id, rev: 31 })),
    ...over,
  };
  return api as never;
}

let n = 0;
const make = (api: KeepApi, extra: { notify?: (m: string) => void } = {}) => {
  const db = createKeepDb(`keep-test-${n++}`);
  const store = createRoot(() => createKeepStore({ api, db, ...extra }));
  return { store, db };
};
const flush = () => new Promise((r) => setTimeout(r, 0));

afterEach(() => vi.useRealTimers());

describe("sync", () => {
  it("loads from cursor 0 and keeps the answer's cursor", async () => {
    const api = fakeApi({ changes: vi.fn(async () => changes([note({ id: "a", rev: 3 }), note({ id: "b", rev: 4 })], "4")) });
    const { store } = make(api);
    await store.load();
    expect(api.changes).toHaveBeenCalledWith("0");
    expect(store.all().map((x) => x.id).sort()).toEqual(["a", "b"]);
    expect(store.cursor()).toBe("4");
    await store.sync();
    expect(api.changes).toHaveBeenLastCalledWith("4");
  });

  it("loops while hasMore, advancing the cursor each page", async () => {
    const api = fakeApi({
      changes: vi
        .fn()
        .mockResolvedValueOnce(changes([note({ id: "a", rev: 1 })], "1", { hasMore: true }))
        .mockResolvedValueOnce(changes([note({ id: "b", rev: 2 })], "2", { hasMore: true }))
        .mockResolvedValueOnce(changes([note({ id: "c", rev: 3 })], "3")),
    });
    const { store } = make(api);
    await store.sync();
    expect(api.changes.mock.calls.map((c) => c[0])).toEqual(["0", "1", "2"]);
    expect(store.all()).toHaveLength(3);
    expect(store.cursor()).toBe("3");
  });

  it("applies removals and replaces notes with newer revisions only", async () => {
    const api = fakeApi({
      changes: vi
        .fn()
        .mockResolvedValueOnce(changes([note({ id: "a", title: "v2", rev: 5 }), note({ id: "b", rev: 2 })], "5"))
        .mockResolvedValueOnce(changes([note({ id: "a", title: "stale", rev: 4 })], "6", { removed: ["b"] })),
    });
    const { store } = make(api);
    await store.sync();
    await store.sync();
    expect(store.all().map((x) => `${x.id}:${x.title}`)).toEqual(["a:v2"]);
  });

  it("on 410 drops the cache and reloads from 0", async () => {
    const api = fakeApi({
      changes: vi
        .fn()
        .mockResolvedValueOnce(changes([note({ id: "old", rev: 1 })], "9"))
        .mockRejectedValueOnce(new ApiError(410, "cursor_expired", "expired"))
        .mockResolvedValueOnce(changes([note({ id: "fresh", rev: 12 })], "12")),
    });
    const { store, db } = make(api);
    await store.sync();
    expect(store.all().map((x) => x.id)).toEqual(["old"]);
    await store.sync();
    expect(api.changes.mock.calls.map((c) => c[0])).toEqual(["0", "9", "0"]);
    expect(store.all().map((x) => x.id)).toEqual(["fresh"]);
    expect(store.cursor()).toBe("12");
    expect((await db.load()).notes.map((x) => x.id)).toEqual(["fresh"]);
  });

  it("shows the cache first, then syncs", async () => {
    const api = fakeApi({ changes: vi.fn(async () => changes([note({ id: "a", title: "new" , rev: 8 })], "8")) });
    const { store, db } = make(api);
    await db.putNotes([note({ id: "a", title: "cached", rev: 2 }), note({ id: "z", rev: 1 })]);
    await db.setCursor("2");
    const gate = deferred<Changes>();
    api.changes.mockImplementationOnce(() => gate.promise);
    const loading = store.load();
    await flush();
    await flush();
    expect(store.all().map((x) => x.title).sort()).toEqual(["", "cached"]);
    gate.resolve(changes([note({ id: "a", title: "new", rev: 8 })], "8"));
    await loading;
    expect(api.changes).toHaveBeenCalledWith("2");
    expect(store.note("a")?.title).toBe("new");
  });

  it("persists notes and the cursor for the next visit", async () => {
    const api = fakeApi({ changes: vi.fn(async () => changes([note({ id: "a", rev: 3 })], "3")) });
    const { store, db } = make(api);
    await store.sync();
    await flush();
    const cache = await db.load();
    expect(cache.cursor).toBe("3");
    expect(cache.notes.map((x) => x.id)).toEqual(["a"]);
  });

  it("keeps showing the cache when a sync fails", async () => {
    const api = fakeApi({
      changes: vi.fn().mockResolvedValueOnce(changes([note({ id: "a", rev: 1 })], "1")).mockRejectedValueOnce(new ApiError(0, "network", "offline")),
    });
    const { store } = make(api);
    await store.sync();
    await store.sync();
    expect(store.status()).toBe("error");
    expect(store.error()).toBe("offline");
    expect(store.all()).toHaveLength(1);
  });

  it("shares one run between overlapping calls", async () => {
    const gate = deferred<Changes>();
    const api = fakeApi({ changes: vi.fn().mockImplementationOnce(() => gate.promise).mockResolvedValue(changes([], "5")) });
    const { store } = make(api);
    const a = store.sync();
    const b = store.sync();
    gate.resolve(changes([], "5"));
    await Promise.all([a, b]);
    // The second call asked for one more run after the first.
    expect(api.changes.mock.calls.length).toBe(2);
  });
});

describe("optimistic writes", () => {
  async function seeded(api: KeepApi, notify?: (m: string) => void) {
    const made = make(api, { notify });
    await made.store.sync();
    return made.store;
  }

  it("shows a change at once and takes the server's answer when it arrives", async () => {
    const gate = deferred<KeepNote>();
    const api = fakeApi({ changes: vi.fn(async () => changes([note({ id: "a", title: "one", rev: 1 })], "1")), patch: vi.fn(() => gate.promise) });
    const store = await seeded(api);
    const p = store.update("a", { title: "two", color: "red" });
    expect(store.note("a")).toMatchObject({ title: "two", color: "red" });
    gate.resolve(note({ id: "a", title: "two", color: "red", rev: 9 }));
    await p;
    expect(store.note("a")?.rev).toBe(9);
  });

  it("sends writes to one note in order", async () => {
    const order: string[] = [];
    const gate = deferred<void>();
    const api = fakeApi({
      changes: vi.fn(async () => changes([note({ id: "a", rev: 1 })], "1")),
      patch: vi.fn(async (id: string, i: NoteInput) => {
        if (i.title === "first") await gate.promise;
        order.push(i.title ?? "");
        return note({ id, title: i.title ?? "", rev: 5 });
      }),
    });
    const store = await seeded(api);
    const p1 = store.update("a", { title: "first" });
    const p2 = store.update("a", { title: "second" });
    await flush();
    expect(order).toEqual([]);
    gate.resolve();
    await Promise.all([p1, p2]);
    expect(order).toEqual(["first", "second"]);
    expect(store.note("a")?.title).toBe("second");
  });

  it("ignores sync data for a note with a write in flight", async () => {
    const gate = deferred<KeepNote>();
    const api = fakeApi({
      changes: vi
        .fn()
        .mockResolvedValueOnce(changes([note({ id: "a", title: "one", rev: 1 })], "1"))
        .mockResolvedValueOnce(changes([note({ id: "a", title: "one", rev: 2 })], "2")),
      patch: vi.fn(() => gate.promise),
    });
    const store = await seeded(api);
    const p = store.update("a", { title: "mine" });
    await store.sync();
    expect(store.note("a")?.title).toBe("mine");
    gate.resolve(note({ id: "a", title: "mine", rev: 3 }));
    await p;
    expect(store.note("a")).toMatchObject({ title: "mine", rev: 3 });
  });

  it("restores the earlier copy and says so when the server refuses", async () => {
    const notify = vi.fn();
    const api = fakeApi({
      changes: vi.fn(async () => changes([note({ id: "a", title: "one", rev: 1 })], "1")),
      patch: vi.fn(async () => {
        throw new ApiError(502, "lucina_unreachable", "Lucina is offline right now.");
      }),
    });
    const store = await seeded(api, notify);
    await store.update("a", { title: "two" });
    expect(store.note("a")?.title).toBe("one");
    expect(notify).toHaveBeenCalledWith("Lucina is offline right now.");
  });

  it("drops a note the server says is gone", async () => {
    const api = fakeApi({
      changes: vi.fn(async () => changes([note({ id: "a", rev: 1 })], "1")),
      patch: vi.fn(async () => {
        throw new ApiError(404, "note_not_found", "no such note");
      }),
    });
    const store = await seeded(api, vi.fn());
    await store.update("a", { title: "x" });
    expect(store.note("a")).toBeUndefined();
  });

  it("creates with a temporary id, then swaps in the server's note and keeps aliasing the old id", async () => {
    const gate = deferred<KeepNote>();
    const api = fakeApi({ create: vi.fn(() => gate.promise) });
    const { store } = make(api);
    const made = store.create({ title: "Draft", content: "text" });
    expect(made.id.startsWith("tmp_")).toBe(true);
    expect(store.note(made.id)).toMatchObject({ title: "Draft", content: "text" });
    gate.resolve(note({ id: "n_real", title: "Draft", content: "text", rev: 7 }));
    const saved = await made.saved;
    expect(saved?.id).toBe("n_real");
    expect(store.all().map((x) => x.id)).toEqual(["n_real"]);
    expect(store.note(made.id)?.id).toBe("n_real");
  });

  it("keeps edits made while the create is in flight", async () => {
    const gate = deferred<KeepNote>();
    const api = fakeApi({
      create: vi.fn(() => gate.promise),
      patch: vi.fn(async (id: string, i: NoteInput) => note({ id, title: i.title ?? "", rev: 8 })),
    });
    const { store } = make(api);
    const made = store.create({ title: "Draft" });
    const edit = store.update(made.id, { title: "Edited" });
    gate.resolve(note({ id: "n_real", title: "Draft", rev: 7 }));
    await made.saved;
    expect(store.note("n_real")?.title).toBe("Edited");
    await edit;
    expect(api.patch).toHaveBeenCalledWith("n_real", { title: "Edited" });
    expect(store.note("n_real")?.rev).toBe(8);
  });

  it("removes a note whose create failed, and says so", async () => {
    const notify = vi.fn();
    const api = fakeApi({
      create: vi.fn(async () => {
        throw new ApiError(0, "network", "Can't reach the server.");
      }),
    });
    const { store } = make(api, { notify });
    const made = store.create({ title: "x" });
    expect(await made.saved).toBeNull();
    expect(store.all()).toHaveLength(0);
    expect(notify).toHaveBeenCalled();
  });

  it("archives, trashes and restores through the batch endpoint", async () => {
    const api = fakeApi({
      changes: vi.fn(async () => changes([note({ id: "a", rev: 1, pinned: true }), note({ id: "b", rev: 1 })], "1")),
      batch: vi.fn(async (b) => b.ids.map((id: string) => note({ id, rev: 9, archived: !!b.set?.archived, ...(b.trash ? { deletedAt: "2026-10-27T10:00:00.000Z" } : {}) }))),
    });
    const store = await seeded(api);
    await store.setMany(["a", "b"], { archived: true });
    expect(api.batch).toHaveBeenCalledWith({ ids: ["a", "b"], set: { archived: true } });
    expect(store.note("a")?.archived).toBe(true);
    const p = store.trash(["a"]);
    expect(store.note("a")?.deletedAt).toBeTruthy();
    await p;
    expect(api.batch).toHaveBeenLastCalledWith({ ids: ["a"], trash: true });
    await store.restore(["a"]);
    expect(api.batch).toHaveBeenLastCalledWith({ ids: ["a"], restore: true });
  });

  it("deletes forever and forgets the note at once", async () => {
    const api = fakeApi({ changes: vi.fn(async () => changes([note({ id: "a", rev: 1, deletedAt: "2026-10-01T00:00:00.000Z" })], "1")) });
    const store = await seeded(api);
    const p = store.deleteForever(["a"]);
    expect(store.note("a")).toBeUndefined();
    await p;
    expect(api.batch).toHaveBeenCalledWith({ ids: ["a"], deleteForever: true });
  });

  it("empties the trash with one call", async () => {
    const api = fakeApi({
      changes: vi
        .fn()
        .mockResolvedValueOnce(changes([note({ id: "a", rev: 1, deletedAt: "2026-10-01T00:00:00.000Z" }), note({ id: "b", rev: 2 })], "2"))
        .mockResolvedValue(changes([], "3", { removed: ["a"] })),
    });
    const store = await seeded(api);
    await store.emptyTrash();
    expect(api.emptyTrash).toHaveBeenCalledTimes(1);
    expect(store.all().map((x) => x.id)).toEqual(["b"]);
  });
});

describe("labels", () => {
  it("lists the registry and the tags in use, alphabetical, with live counts", async () => {
    const api = fakeApi({
      changes: vi.fn(async () => changes([note({ id: "a", tags: ["godlevel"], rev: 1 }), note({ id: "b", tags: ["godlevel", "stray"], rev: 2 }), note({ id: "c", tags: ["chill"], deletedAt: "2026-10-01T00:00:00.000Z", rev: 3 })], "3")),
      labels: vi.fn(async () => [
        { tag: "godlevel", name: "GodLevel", count: 2 },
        { tag: "chill", name: "Chill", count: 0 },
        { tag: "film", name: "film", count: 0 },
      ]),
    });
    const { store } = make(api);
    await store.sync();
    expect(store.labels().map((l) => `${l.name}:${l.count}`)).toEqual(["Chill:0", "film:0", "GodLevel:2", "stray:1"]);
    expect(store.labelName("godlevel")).toBe("GodLevel");
    expect(store.labelName("nothing")).toBe("nothing");
  });

  it("adds a new label to notes by its display name", async () => {
    const api = fakeApi({ changes: vi.fn(async () => changes([note({ id: "a", rev: 1 })], "1")) });
    const { store } = make(api);
    await store.sync();
    const p = store.addLabel(["a"], "  #Fisica   2 ");
    expect(store.note("a")?.tags).toEqual(["fisica 2"]);
    expect(store.labels().map((l) => l.name)).toContain("Fisica 2");
    await p;
    expect(api.batch).toHaveBeenCalledWith({ ids: ["a"], addLabels: ["Fisica 2"] });
  });

  it("deleting a label strips it from the cached notes", async () => {
    const api = fakeApi({
      changes: vi
        .fn()
        .mockResolvedValueOnce(changes([note({ id: "a", tags: ["x", "y"], rev: 1 })], "1"))
        .mockResolvedValue(changes([note({ id: "a", tags: ["y"], rev: 2 })], "2")),
    });
    const { store } = make(api);
    await store.sync();
    await store.deleteLabel("x");
    expect(store.note("a")?.tags).toEqual(["y"]);
    expect(api.deleteLabel).toHaveBeenCalledWith("x");
  });
});

describe("applyInput", () => {
  const base = note({ id: "a", title: "T", content: "c", tags: ["x"], reminder: { at: "2026-10-30T09:00:00.000Z", lucinaId: "r1" } });
  const t = "2026-10-27T12:00:00.000Z";

  it("applies the fields the way the server will", () => {
    expect(applyInput(base, { color: "teal", pinned: true, archived: true, position: 5 }, t)).toMatchObject({ color: "teal", pinned: true, archived: true, position: 5, updatedAt: t });
    expect(applyInput(base, { labels: ["Chill", "chill", " #GodLevel "] }, t).tags).toEqual(["chill", "godlevel"]);
    expect(applyInput(base, { reminder: null }, t).reminder).toBeUndefined();
    expect(applyInput(base, { reminder: { at: "2026-11-01T09:00:00.000Z", recurrence: "daily" } }, t).reminder).toEqual({ at: "2026-11-01T09:00:00.000Z", recurrence: "daily" });
  });

  it("derives a list note's content from its items", () => {
    const items = [{ id: "i1", text: "eggs", checked: false, indent: 0 as const }, { id: "i2", text: "milk", checked: true, indent: 1 as const }];
    const n = applyInput(base, { items }, t);
    expect(n.items).toEqual(items);
    expect(n.content).toBe("- [ ] eggs\n  - [x] milk");
  });

  it("converts between text and list", () => {
    const list = applyInput(base, { convert: "list" }, t);
    expect(list.items?.map((i) => i.text)).toEqual(["c"]);
    const text = applyInput(list, { convert: "text" }, t);
    expect(text.items).toBeUndefined();
    expect(text.content).toBe("c");
  });

  it("parses checkbox lines back into items", () => {
    expect(contentToItems("- [x] done\n- [ ] todo\nplain\n\n  - [ ] sub").map((i) => [i.text, i.checked, i.indent])).toEqual([
      ["done", true, 0],
      ["todo", false, 0],
      ["plain", false, 0],
      ["sub", false, 1],
    ]);
    expect(itemsToContent(contentToItems("- [x] a"))).toBe("- [x] a");
  });
});

describe("auto sync", () => {
  beforeEach(() => vi.useFakeTimers());

  it("syncs every 60 seconds while visible, on focus and when the tab shows again", () => {
    const sync = vi.fn(async () => undefined);
    const stop = startAutoSync({ sync });
    vi.advanceTimersByTime(60_000);
    expect(sync).toHaveBeenCalledTimes(1);
    window.dispatchEvent(new Event("focus"));
    expect(sync).toHaveBeenCalledTimes(2);
    document.dispatchEvent(new Event("visibilitychange"));
    expect(sync).toHaveBeenCalledTimes(3);
    stop();
    vi.advanceTimersByTime(120_000);
    window.dispatchEvent(new Event("focus"));
    expect(sync).toHaveBeenCalledTimes(3);
  });

  it("does not sync while the tab is hidden", () => {
    const sync = vi.fn(async () => undefined);
    const stop = startAutoSync({ sync });
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "hidden" });
    vi.advanceTimersByTime(180_000);
    expect(sync).not.toHaveBeenCalled();
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "visible" });
    stop();
  });
});
