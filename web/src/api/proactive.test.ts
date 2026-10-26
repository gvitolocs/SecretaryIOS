import { describe, expect, it } from "vitest";
import { outreachFromSearch } from "./proactive";
import { parseItem, safeRoute } from "./timeline";

const ID = "po_01K85Z3Q4X5Y6Z7A8B9C0D1E2F";

describe("proactive timeline items", () => {
  it("parses the proactive meta and drops anything unsafe", () => {
    const item = parseItem({
      id: "01K85Z3Q4X5Y6Z7A8B9C0D1E2G",
      ts: "2026-10-22T11:50:00.000Z",
      channel: "other",
      role: "lucina",
      text: "Tra un'ora hai CRAFT-IT4SD.",
      meta: {
        kind: "proactive",
        topic: "meeting_prep",
        priority: "high",
        outreachId: ID,
        why: "starts in 60 min · score 0.83",
        actions: [
          { id: "open_note", label: "Apri la nota", route: "/notes/prep-20261022-craft" },
          { id: "evil", label: "Evil", route: "https://evil.example" },
          { id: "proto", label: "Proto", route: "//evil.example" },
          { id: "useful", label: "Utile", feedback: true },
          { id: "not_now", label: "Non ora", feedback: true },
          { id: "stop", label: "Basta così", feedback: true },
          { id: "delete_all", label: "Delete", feedback: true },
        ],
      },
    });
    const p = item?.meta?.proactive;
    expect(item?.meta?.kind).toBe("proactive");
    expect(p?.priority).toBe("high");
    expect(p?.topic).toBe("meeting_prep");
    expect(p?.actions.map((a) => a.id)).toEqual(["open_note", "useful", "not_now", "stop"]);
    expect(p?.actions[0]?.route).toBe("/notes/prep-20261022-craft");
  });

  it("ignores a proactive meta without a valid outreach id", () => {
    const item = parseItem({
      id: "1",
      ts: 1,
      channel: "other",
      role: "lucina",
      text: "x",
      meta: { kind: "proactive", outreachId: "nope", priority: "urgent" },
    });
    expect(item?.meta).toBeUndefined();
    const ok = parseItem({ id: "1", ts: 1, channel: "other", role: "lucina", text: "x", meta: { kind: "proactive", outreachId: ID, priority: "urgent" } });
    expect(ok?.meta?.proactive?.priority).toBe("normal");
    expect(ok?.meta?.proactive?.actions).toEqual([]);
  });

  it("routes and deep links", () => {
    expect(safeRoute("/notes/x")).toBe("/notes/x");
    expect(safeRoute("/a b")).toBeUndefined();
    expect(safeRoute("javascript:alert(1)")).toBeUndefined();
    expect(safeRoute("/\\evil")).toBeUndefined();
    expect(outreachFromSearch(`?outreach=${ID}`)).toBe(ID);
    expect(outreachFromSearch("?outreach=po_bad")).toBeNull();
    expect(outreachFromSearch("")).toBeNull();
  });
});
