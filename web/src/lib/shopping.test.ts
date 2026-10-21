import { describe, expect, it } from "vitest";
import { nextStatus, normalizeList, togglePayload } from "./shopping";

describe("normalizeList", () => {
  it("falls back across name fields and keeps ids", () => {
    const items = normalizeList([
      { id: "a", name: "Milk", quantity: 2, checked: false },
      { id: 7, rawName: "eggs x6" },
      { uuid: "u1", product: { name: "Beans" }, quantity: "1", unit: "kg", status: "bought" },
      { title: "No id" },
      "garbage",
      null,
    ]);
    expect(items.map((i) => [i.id, i.name, i.quantity, i.checked, i.hasId])).toEqual([
      ["a", "Milk", "2", false, true],
      ["7", "eggs x6", null, false, true],
      ["u1", "Beans", "1 kg", true, true],
      ["local-3", "No id", null, false, false],
    ]);
  });

  it("reads the server's item shape (product string, needed/purchased)", () => {
    const [needed, bought] = normalizeList([
      { id: "3d0b", product: "milk", rawName: "latte", category: "dairy", status: "needed" },
      { id: "9a1c", product: "bread", status: "purchased" },
    ]);
    expect(needed).toMatchObject({ id: "3d0b", name: "latte", checked: false, toggleField: "status" });
    expect(bought).toMatchObject({ name: "bread", checked: true });
    expect(togglePayload(needed!, true)).toEqual({ status: "purchased" });
    expect(togglePayload(bought!, false)).toEqual({ status: "needed" });
  });

  it("accepts {items: [...]} defensively", () => {
    expect(normalizeList({ items: [{ id: "x", name: "Tea" }] })).toHaveLength(1);
    expect(normalizeList({ nope: true })).toEqual([]);
  });
});

describe("toggle payloads", () => {
  it("uses checked when the item has a checked boolean", () => {
    const [item] = normalizeList([{ id: "1", name: "a", checked: false }]);
    expect(togglePayload(item!, true)).toEqual({ checked: true });
  });

  it("uses the item's status vocabulary when it has a status", () => {
    const [item] = normalizeList([{ id: "1", name: "a", status: "TO_PURCHASE" }]);
    expect(item!.toggleField).toBe("status");
    expect(togglePayload(item!, true)).toEqual({ status: "PURCHASED" });
    expect(nextStatus("done", false)).toBe("pending");
    expect(nextStatus("open", true)).toBe("bought");
    expect(nextStatus("needed", true)).toBe("purchased");
    expect(nextStatus("purchased", false)).toBe("needed");
    expect(nextStatus(null, true)).toBe("purchased");
  });
});
