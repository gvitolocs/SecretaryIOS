import { describe, expect, it } from "vitest";
import { applyOps, displayName, parseItem, rebaseOps, type ApplyContext, type Op, type ShopItem } from "./shoppingOps";

function item(p: Partial<ShopItem> & { id: string; name: string }): ShopItem {
  return {
    objectId: p.catalogId ?? null,
    kind: p.catalogId ? "catalog" : null,
    names: {},
    catalogId: null,
    typedName: p.name,
    needsTranslation: false,
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
    const res = applyOps([], [{ opId: "1", type: "add", rowId: "c1", itemId: "banana" }], ctx({
      catalog: () => ({ name: "Banane", names: { it: "Banane", en: "Bananas" }, kind: "catalog", section: "produce", icon: "🍌" }),
    }));
    expect(res.items[0]).toMatchObject({
      id: "c1", objectId: "banana", kind: "catalog", catalogId: "banana", name: "Banane", section: "produce", icon: "🍌", status: "needed",
    });
    expect(res.items[0]!.names.en).toBe("Bananas");
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

describe("item objects", () => {
  const milkObj = item({ id: "m", name: "Latte", catalogId: "milk", names: { it: "Latte", en: "Milk", da: "Mælk" }, status: "recent" });
  const soap = item({ id: "s", name: "Detersivo piatti", objectId: "u_1", kind: "custom", names: { it: "Detersivo piatti", en: "Dish soap", da: "Opvaskemiddel" } });

  it("text in any language finds the row of its object; a recent row goes back to needed", () => {
    const res = applyOps([milkObj, soap], [
      { opId: "1", type: "add", rowId: "r1", text: "MÆLK" },
      { opId: "2", type: "add", rowId: "r2", text: "dish soap" },
      { opId: "3", type: "add", rowId: "r3", itemId: "milk" },
    ], ctx());
    expect(res.items).toHaveLength(2);
    expect(res.items.find((i) => i.id === "m")!.status).toBe("needed");
  });

  it("a resolver maps typed text to a known object id", () => {
    const res = applyOps([milkObj], [{ opId: "1", type: "add", rowId: "r1", text: "whole milk" }], ctx({ resolve: (t) => (t === "whole milk" ? "milk" : undefined) }));
    expect(res.items).toHaveLength(1);
  });

  it("an old outbox add kept its row id in itemId (a UUID)", () => {
    const rowUuid = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
    const res = applyOps([], [{ opId: "1", type: "add", itemId: rowUuid, catalogId: "banana", name: "Banane" }], ctx());
    expect(res.items[0]).toMatchObject({ id: rowUuid, objectId: "banana", catalogId: "banana" });
  });

  it("edit_item changes every row of the object; rename on a custom row edits the UI language's name", () => {
    const edited = applyOps([soap], [
      { opId: "1", type: "edit_item", itemId: "u_1", names: { en: "Dishwashing liquid" }, icon: "🫧", section: "household" },
    ], ctx({ lang: "en" })).items[0]!;
    expect(edited).toMatchObject({ name: "Dishwashing liquid", icon: "🫧", section: "household" });
    expect(edited.names).toEqual({ it: "Detersivo piatti", en: "Dishwashing liquid", da: "Opvaskemiddel" });
    const renamed = applyOps([soap], [{ opId: "2", type: "rename", itemId: "s", name: "Opvask" }], ctx({ lang: "da" })).items[0]!;
    expect(renamed.names.da).toBe("Opvask");
    expect(renamed.objectId).toBe("u_1");
  });

  it("parses rows from the server (item object) and from the cache", () => {
    const fromServer = parseItem({
      id: "r", itemId: "u_1", catalogId: null, name: "Dish soap", typedName: "detersivo piatti", status: "needed",
      item: { id: "u_1", kind: "custom", names: { it: "Detersivo piatti", en: "Dish soap" }, needsTranslation: true },
    })!;
    expect(fromServer).toMatchObject({ objectId: "u_1", kind: "custom", typedName: "detersivo piatti", needsTranslation: true });
    expect(displayName(fromServer, "it")).toBe("Detersivo piatti");
    expect(displayName(fromServer, "da")).toBe("Dish soap");
    const cached = parseItem(JSON.parse(JSON.stringify(fromServer)))!;
    expect(cached).toEqual(fromServer);
  });
});

describe("rebaseOps", () => {
  it("gives every op a new id and drops ops on rows that are gone or no-ops", () => {
    const pending: Op[] = [
      { opId: "a", type: "purchase", itemId: "m" },
      { opId: "b", type: "set_spec", itemId: "gone", spec: "x2" },
      { opId: "c", type: "purchase", itemId: "b" }, // already recent on the server
      { opId: "d", type: "add", rowId: "n1", text: "Uova" },
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
