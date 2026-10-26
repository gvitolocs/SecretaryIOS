import { describe, expect, it } from "vitest";
import { applyOps, parseItem, rebaseOps, type ApplyContext, type Op, type ShopItem } from "./shoppingOps";

function item(p: Partial<ShopItem> & { id: string; name: string }): ShopItem {
  return {
    catalogId: null,
    spec: "",
    quantity: null,
    unit: null,
    section: null,
    icon: null,
    status: "needed",
    position: 1,
    updatedAt: "",
    updatedBy: "",
    ...p,
  };
}

function ctx(extra: Partial<ApplyContext> = {}): ApplyContext {
  let n = 0;
  return { listId: "default", now: "2026-10-21T12:00:00Z", by: "web", newId: () => `new-${++n}`, ...extra };
}

const milk = item({ id: "m", name: "Latte", catalogId: "milk", position: 1 });
const bread = item({ id: "b", name: "Pane", status: "recent", position: 2 });

describe("applyOps", () => {
  it("applies status transitions and stamps the source", () => {
    const res = applyOps([milk, bread], [
      { opId: "1", type: "purchase", itemId: "m" },
      { opId: "2", type: "need", itemId: "b" },
    ], ctx());
    expect(res.items.find((i) => i.id === "m")).toMatchObject({ status: "recent", updatedBy: "web" });
    expect(res.items.find((i) => i.id === "b")).toMatchObject({ status: "needed", position: 1 });
    expect(res.applied).toEqual(["1", "2"]);
  });

  it("dedupes op ids across calls and within a batch", () => {
    const seen = new Set<string>();
    const first = applyOps([milk], [{ opId: "x", type: "set_spec", itemId: "m", spec: "x2" }], ctx({ seen }));
    expect(first.items[0]!.spec).toBe("x2");
    const again = applyOps(first.items, [
      { opId: "x", type: "set_spec", itemId: "m", spec: "x9" },
      { opId: "y", type: "set_spec", itemId: "m", spec: "bio" },
      { opId: "y", type: "set_spec", itemId: "m", spec: "nope" },
    ], ctx({ seen }));
    expect(again.duplicates).toEqual(["x", "y"]);
    expect(again.applied).toEqual(["y"]);
    expect(again.items[0]!.spec).toBe("bio");
  });

  it("add revives a recent row instead of duplicating it (catalog id or folded name)", () => {
    const res = applyOps([milk, bread], [
      { opId: "1", type: "add", name: "pané", spec: "integrale" },
      { opId: "2", type: "add", catalogId: "milk", name: "Milk" },
    ], ctx());
    expect(res.items).toHaveLength(2);
    expect(res.items.find((i) => i.id === "b")).toMatchObject({ status: "needed", spec: "integrale" });
  });

  it("add creates a row with the client id and catalog hints", () => {
    const res = applyOps([], [{ opId: "1", type: "add", itemId: "c1", catalogId: "banana", name: "Banane" }], ctx({
      catalog: () => ({ name: "Banane", section: "produce", icon: "🍌" }),
    }));
    expect(res.items[0]).toMatchObject({ id: "c1", catalogId: "banana", section: "produce", icon: "🍌", status: "needed" });
  });

  it("deleted stays deleted, and re-adding makes a new row", () => {
    const removed = applyOps([milk], [{ opId: "1", type: "remove", itemId: "m" }], ctx()).items;
    const res = applyOps(removed, [
      { opId: "2", type: "need", itemId: "m" },
      { opId: "3", type: "set_spec", itemId: "m", spec: "x" },
      { opId: "4", type: "add", itemId: "m", catalogId: "milk", name: "Latte" },
    ], ctx());
    expect(res.items.find((i) => i.id === "m")).toMatchObject({ status: "removed", spec: "" });
    const fresh = res.items.filter((i) => i.status !== "removed");
    expect(fresh).toHaveLength(1);
    expect(fresh[0]!.id).toBe("new-1");
  });

  it("clear_recent removes only recent rows; move removes the row from this list", () => {
    const res = applyOps([milk, bread], [
      { opId: "1", type: "clear_recent" },
      { opId: "2", type: "move", itemId: "m", toListId: "other" },
    ], ctx());
    expect(res.items.map((i) => i.status)).toEqual(["removed", "removed"]);
    const same = applyOps([milk], [{ opId: "3", type: "move", itemId: "m", toListId: "default" }], ctx());
    expect(same.items[0]!.status).toBe("needed");
  });

  it("set_quantity validates numbers", () => {
    const res = applyOps([milk], [{ opId: "1", type: "set_quantity", itemId: "m", quantity: -3, unit: "l" }], ctx());
    expect(res.items[0]).toMatchObject({ quantity: null, unit: "l" });
  });
});

describe("rebaseOps", () => {
  it("gives every op a new id and drops ops on rows that are gone or no-ops", () => {
    const pending: Op[] = [
      { opId: "a", type: "purchase", itemId: "m" },
      { opId: "b", type: "set_spec", itemId: "gone", spec: "x2" },
      { opId: "c", type: "purchase", itemId: "b" }, // already recent on the server
      { opId: "d", type: "add", itemId: "n1", name: "Uova" },
      { opId: "e", type: "set_spec", itemId: "n1", spec: "x6" },
    ];
    const out = rebaseOps([milk, bread], pending, ctx());
    expect(out.map((o) => o.type)).toEqual(["purchase", "add", "set_spec"]);
    expect(out.every((o) => !["a", "b", "c", "d", "e"].includes(o.opId))).toBe(true);
    expect(new Set(out.map((o) => o.opId)).size).toBe(3);
  });
});

describe("parseItem", () => {
  it("rejects rows without id or name and defaults the status", () => {
    expect(parseItem({ name: "x" })).toBeNull();
    expect(parseItem({ id: 7, name: "x", status: "weird" })).toMatchObject({ id: "7", status: "needed" });
  });
});
