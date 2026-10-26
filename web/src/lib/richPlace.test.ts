import { describe, expect, it } from "vitest";
import { parseItem } from "../api/timeline";
import { MAX_PLACES, parsePlace, parseRich } from "./rich";

const hackNight = {
  type: "place",
  title: "Hack Night: Rust meets AI",
  address: "Folkehuset Møllestien, Aarhus",
  lat: 56.1611,
  lng: 10.2006,
  when: "2026-10-27T17:00:00.000Z",
  end: "2026-10-27T20:00:00.000Z",
  eventId: "iphone:ABC-123",
  kind: "event",
};

function blocks(...b: unknown[]) {
  return parseRich({ format: "v1", bubbles: ["Here"], blocks: b })!.blocks;
}

describe("place blocks", () => {
  it("keeps a valid block with every field", () => {
    expect(blocks(hackNight)).toEqual([hackNight]);
  });

  it("drops a title-only block and one without a title", () => {
    expect(blocks({ type: "place", title: "Somewhere" })).toEqual([]);
    expect(blocks({ type: "place", title: "  ", address: "Aarhus" })).toEqual([]);
    expect(blocks({ type: "place", address: "Aarhus", lat: 56.1, lng: 10.2 })).toEqual([]);
  });

  it("drops a pair with one coordinate, keeping the address", () => {
    expect(blocks({ type: "place", title: "T", address: "Aarhus", lat: 56.1 })).toEqual([
      { type: "place", title: "T", address: "Aarhus", kind: "place" },
    ]);
    // ...and the whole block when the address is missing too
    expect(blocks({ type: "place", title: "T", lng: 10.2 })).toEqual([]);
  });

  it("drops (0,0), out-of-range, non-finite and string coordinates", () => {
    for (const [lat, lng] of [
      [0, 0],
      [91, 10],
      [56, 181],
      [Number.NaN, 10],
      ["56.1", "10.2"],
    ]) {
      expect(blocks({ type: "place", title: "T", address: "Aarhus", lat, lng })).toEqual([
        { type: "place", title: "T", address: "Aarhus", kind: "place" },
      ]);
    }
    // a coordinate pair alone is enough
    expect(blocks({ type: "place", title: "T", lat: -33.9, lng: 151.2 })).toEqual([
      { type: "place", title: "T", lat: -33.9, lng: 151.2, kind: "place" },
    ]);
  });

  it("caps lengths and drops bad times and ids", () => {
    const [b] = blocks({
      type: "place",
      title: `${"x".repeat(200)}\nsecond line`,
      address: "a".repeat(300),
      when: "tomorrow at 5",
      end: "2026-10-27",
      eventId: "e".repeat(201),
      kind: "party",
    });
    expect(b).toMatchObject({ type: "place", kind: "place" });
    const p = b as { title: string; address: string };
    expect(p.title.length).toBe(120);
    expect(p.title).not.toContain("\n");
    expect(p.address.length).toBe(200);
    expect(b).not.toHaveProperty("when");
    expect(b).not.toHaveProperty("end");
    expect(b).not.toHaveProperty("eventId");
  });

  it(`keeps at most ${MAX_PLACES} place blocks, within the 8-block limit`, () => {
    const many = Array.from({ length: 5 }, (_, i) => ({ type: "place", title: `P${i}`, address: `A${i}` }));
    const out = blocks(...many, { type: "quote", text: "after" });
    expect(out.map((b) => (b.type === "place" ? b.title : b.type))).toEqual(["P0", "P1", "P2", "quote"]);
    const mixed = blocks(
      ...Array.from({ length: 7 }, (_, i) => ({ type: "quote", text: `q${i}` })),
      { type: "place", title: "P", address: "A" },
      { type: "place", title: "Q", address: "A" },
    );
    expect(mixed).toHaveLength(8);
    expect(mixed.at(-1)).toMatchObject({ type: "place", title: "P" });
  });

  it("parses meta.place on proactive items only", () => {
    const { type: _type, ...place } = hackNight;
    expect(parsePlace(place)).toEqual(place);
    const meta = { kind: "proactive", outreachId: "po_01JB0000000000000000000000", actions: [], place };
    expect(parseItem({ id: "1", ts: 1, role: "lucina", channel: "web", text: "Soon", meta })?.meta?.place).toEqual(place);
    const bad = { ...meta, place: { title: "No where" } };
    expect(parseItem({ id: "2", ts: 1, role: "lucina", channel: "web", text: "Soon", meta: bad })?.meta?.place).toBeUndefined();
    const reminder = { kind: "reminder", place };
    expect(parseItem({ id: "3", ts: 1, role: "lucina", channel: "web", text: "Soon", meta: reminder })?.meta?.place).toBeUndefined();
  });
});
