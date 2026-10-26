import { waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it } from "vitest";
import { setLocale } from "../i18n";
import { OUTBOX_KEY } from "../lib/shoppingOutbox";
import { stubFetch } from "../test/fetch";
import { fakeShoppingServer, testItem, type FakeServer } from "../views/shopping/fakeServer";
import {
  addText,
  editItem,
  enqueue,
  flushOutbox,
  initShopping,
  itemIcon,
  itemSection,
  outboxSnapshot,
  resetShopping,
  shop,
  tapItem,
  undoLast,
} from "./shopping";

let srv: FakeServer;

function setup(): void {
  srv = fakeShoppingServer({
    default: {
      name: "Casa",
      theme: "home",
      items: [
        testItem({ id: "milk-row", name: "Latte", catalogId: "milk", section: "dairy" }),
        testItem({ id: "bread-row", name: "Pane", status: "recent" }),
      ],
    },
  });
  stubFetch(srv.handler);
}

async function booted(): Promise<void> {
  initShopping();
  await waitFor(() => expect(shop.items.length).toBe(2));
  await waitFor(() => expect(shop.pending).toBe(0));
}

const status = (id: string) => shop.items.find((i) => i.id === id)?.status;

describe("shopping state", () => {
  beforeEach(() => {
    setLocale("it");
    resetShopping();
    setup();
  });

  it("applies a tap optimistically and confirms it with one ops POST", async () => {
    await booted();
    tapItem("milk-row");
    expect(status("milk-row")).toBe("recent");
    await waitFor(() => expect(shop.pending).toBe(0));
    const posts = srv.opsPosts();
    expect(posts).toHaveLength(1);
    expect(posts[0]).toMatchObject({ listId: "default", expectedRevision: 1, ops: [{ type: "purchase", itemId: "milk-row" }] });
    expect(srv.calls.at(-1)?.headers["x-secretary-client"]).toBe("web");
    expect(shop.revision).toBe(2);
    expect(status("milk-row")).toBe("recent");
  });

  it("undo sends the inverse op", async () => {
    await booted();
    tapItem("milk-row");
    expect(shop.undo?.message).toBe("Latte comprato");
    undoLast();
    expect(status("milk-row")).toBe("needed");
    expect(shop.undo).toBeNull();
    await waitFor(() => expect(shop.pending).toBe(0));
    const types = srv.opsPosts().flatMap((p) => p.ops.map((o) => o.type));
    expect(types).toEqual(["purchase", "need"]);
  });

  it("free text resolves to the catalog, else becomes a custom item", async () => {
    await booted();
    await waitFor(() => expect(shop.catalog.items.length).toBeGreaterThan(0));
    addText("caffe");
    addText("Candele");
    addText("pane"); // recent -> back on the list, no duplicate
    expect(shop.items.find((i) => i.catalogId === "coffee")).toMatchObject({ name: "Caffè", section: "pantry", icon: "☕" });
    expect(shop.items.find((i) => i.name === "Candele")?.catalogId).toBeNull();
    expect(status("bread-row")).toBe("needed");
    expect(shop.items).toHaveLength(4);
    await waitFor(() => expect(shop.pending).toBe(0));
    // The client's item ids were kept by the server.
    expect(srv.lists.get("default")!.items.map((i) => i.name)).toContain("Caffè");
    expect(shop.items).toHaveLength(4);
  });

  it("rows with no catalog id (pre-catalog, old server) take icon and section from their name", async () => {
    await booted();
    await waitFor(() => expect(shop.catalog.items.length).toBeGreaterThan(0));
    const row = (name: string, extra: Record<string, unknown> = {}) => ({ name, catalogId: null, icon: null, section: null, ...extra });
    expect([itemIcon(row("milk")), itemSection(row("milk"))]).toEqual(["🥛", "dairy"]);
    expect([itemIcon(row("Mælk")), itemSection(row("LATTE"))]).toEqual(["🥛", "dairy"]);
    expect([itemIcon(row("espresso")), itemSection(row("espresso"))]).toEqual(["☕", "pantry"]);
    expect([itemIcon(row("Candele")), itemSection(row("Candele"))]).toEqual([null, "other"]);
    expect(itemIcon(row("the milk"))).toBeNull();
    expect(itemIcon(row("milk", { icon: "🍼", section: "pantry" }))).toBe("🍼");
    expect(itemSection(row("milk", { section: "pantry" }))).toBe("pantry");
    expect(itemIcon(row("whatever", { catalogId: "coffee" }))).toBe("☕");
  });

  it("keeps ops in the outbox while offline, persists them and replays them with the same ids", async () => {
    await booted();
    srv.offline = true;
    tapItem("milk-row");
    editItem("bread-row", { spec: "integrale" });
    await waitFor(() => expect(shop.online).toBe(false));
    expect(status("milk-row")).toBe("recent");
    expect(shop.pending).toBe(2);

    const stored = localStorage.getItem(OUTBOX_KEY);
    expect(stored).toBeTruthy();
    const ids = outboxSnapshot().map((e) => e.op.opId);
    expect(JSON.parse(stored!).map((e: { op: { opId: string } }) => e.op.opId)).toEqual(ids);

    // "Reload": forget memory, keep storage, start again online.
    resetShopping();
    localStorage.setItem(OUTBOX_KEY, stored!);
    srv.offline = false;
    srv.calls.length = 0;
    initShopping();
    // The cached state is gone (reset clears it), but the queued ops show at once on the fresh state.
    await waitFor(() => expect(shop.pending).toBe(0));
    const replayed = srv.opsPosts().flatMap((p) => p.ops.map((o) => o.opId));
    expect(replayed).toEqual(ids);
    expect(srv.lists.get("default")!.items.find((i) => i.id === "milk-row")?.status).toBe("recent");
    expect(srv.lists.get("default")!.items.find((i) => i.id === "bread-row")?.spec).toBe("integrale");
    expect(localStorage.getItem(OUTBOX_KEY)).toBeNull();
  });

  it("replaying an op the server already applied is reported as a duplicate and changes nothing", async () => {
    await booted();
    const [op] = enqueue("default", [{ type: "set_spec", itemId: "milk-row", spec: "x2" }]);
    await waitFor(() => expect(shop.pending).toBe(0));
    // A retry after an unclear outcome reuses the id.
    srv.lists.get("default")!.items.find((i) => i.id === "milk-row")!.spec = "changed elsewhere";
    const res = await fetch("/v1/shopping/lists/default/ops", {
      method: "POST",
      body: JSON.stringify({ expectedRevision: 2, ops: [op] }),
    }).then((r) => r.json());
    expect(res).toMatchObject({ applied: [], duplicates: [op!.opId] });
    expect(srv.lists.get("default")!.items.find((i) => i.id === "milk-row")?.spec).toBe("changed elsewhere");
  });

  it("on 409 refetches, rebases pending ops with new ids and drops ops on removed rows", async () => {
    await booted();
    srv.conflictNext = true;
    srv.beforeConflict = () => {
      // Someone deleted the bread on Telegram meanwhile.
      const bread = srv.lists.get("default")!.items.find((i) => i.id === "bread-row")!;
      bread.status = "removed";
    };
    srv.offline = true;
    tapItem("milk-row");
    editItem("bread-row", { spec: "integrale" });
    await waitFor(() => expect(shop.online).toBe(false));
    const original = outboxSnapshot().map((e) => e.op.opId);
    srv.offline = false;
    await flushOutbox();
    await waitFor(() => expect(shop.pending).toBe(0));

    const posts = srv.opsPosts();
    const last = posts.at(-1)!;
    expect(last.ops).toHaveLength(1);
    expect(last.ops[0]).toMatchObject({ type: "purchase", itemId: "milk-row" });
    expect(original).not.toContain(last.ops[0]!.opId);
    expect(last.expectedRevision).toBe(2);
    expect(status("milk-row")).toBe("recent");
    expect(shop.items.find((i) => i.id === "bread-row")).toBeUndefined();
  });
});
