import { describe, expect, it } from "vitest";
import { event, overview } from "../test/serverFixtures";
import {
  attentionFor,
  chartPaths,
  chartScale,
  dayKey,
  dayRelation,
  diskLevel,
  duration,
  filterServices,
  groupByDay,
  healthyByGroup,
  hourTicks,
  seriesStats,
} from "./systemOverview";

describe("system overview helpers", () => {
  it("formats durations", () => {
    const now = Date.parse("2026-10-27T12:00:00Z");
    expect(duration("2026-10-27T11:48:00Z", now, "en")).toBe("12 min");
    expect(duration("2026-10-27T10:00:00Z", now, "en")).toBe("2 hr");
    expect(duration("2026-10-24T12:00:00Z", now, "en")).toBe("3 days");
  });

  it("levels disks at 80 and 90 %", () => {
    expect([79.9, 80, 89.9, 90].map(diskLevel)).toEqual(["ok", "warn", "warn", "crit"]);
  });

  it("groups the timeline by local day, newest first, filtered by kind", () => {
    const now = new Date(2026, 9, 28, 12, 0).getTime();
    const at = (d: number, h: number) => new Date(2026, 9, d, h).toISOString();
    const items = [
      event({ at: at(27, 9), kind: "deploy", title: "b" }),
      event({ at: at(28, 10), kind: "incident", title: "a" }),
      event({ at: at(28, 11), kind: "change", title: "c" }),
      event({ at: at(25, 9), kind: "change", title: "d" }),
    ];
    const days = groupByDay(items, ["incident", "change"]);
    expect(days.map((d) => d.day)).toEqual(["2026-10-28", "2026-10-25"]);
    expect(days[0]!.items.map((i) => i.title)).toEqual(["c", "a"]);
    expect(dayRelation(dayKey(at(28, 1)), now)).toBe("today");
    expect(dayRelation(dayKey(at(27, 23)), now)).toBe("yesterday");
    expect(dayRelation("2026-10-25", now)).toBe("other");
  });

  it("filters services by id, name, host and project", () => {
    const tree = overview().services;
    expect(filterServices(tree, "")).toBe(tree);
    const byName = filterServices(tree, "maps");
    expect(byName.map((g) => g.name)).toEqual(["Lucina"]);
    expect(byName[0]!.projects[0]!.components.map((c) => c.id)).toEqual(["gmaps-server", "lucina-maps"]);
    expect(filterServices(tree, "P1")[0]!.projects[0]!.components.map((c) => c.id)).toEqual(["pokoin-api"]);
    expect(filterServices(tree, "secretary")[0]!.projects[0]!.components).toHaveLength(3);
    expect(filterServices(tree, "nothing-like-this")).toEqual([]);
  });

  it("groups healthy components and finds a component's attention item", () => {
    const o = overview();
    expect(healthyByGroup(o.services).map((g) => [g.group, g.components.map((c) => c.id)])).toEqual([
      ["Lucina", ["secretary-server"]],
      ["Pokoin", ["pokoin-api"]],
    ]);
    expect(attentionFor(o.attention, "gmaps-server")?.key).toBe("catalog:gmaps-server");
    expect(attentionFor(o.attention, "secretary-server")).toBeUndefined();
  });

  it("computes stats and scales", () => {
    expect(seriesStats([null, 3, 1, null, 5, null])).toEqual({ now: 5, min: 1, max: 5 });
    expect(seriesStats([null, null])).toEqual({ now: null, min: null, max: null });
    expect(chartScale([10, 20], { fixed: [30, 100] })).toEqual([30, 100]);
    expect(chartScale([1, 3])).toEqual([0.8, 3.2]);
    expect(chartScale([0.1, 3], { floor: 0 })[0]).toBe(0);
    expect(chartScale([2, 2])).toEqual([1.5, 2.5]);
  });

  it("breaks the line at nulls and closes each area", () => {
    const p = chartPaths([0, 10, null, 10, 0], 100, 10, [0, 10]);
    expect(p.line).toBe("M0 10 L25 0 M75 0 L100 10");
    expect(p.area).toBe("M0 10 L0 10 L25 0 L25 10 Z M75 10 L75 0 L100 10 L100 10 Z");
    expect(p.last).toEqual({ x: 1, y: 1 });
    // A lone point between gaps is drawn as a short flat stroke, without an area.
    const lone = chartPaths([null, 5, null], 100, 10, [0, 10]);
    expect(lone.line).toBe("M25 5 L75 5");
    expect(lone.area).toBe("");
    expect(chartPaths([null], 100, 10, [0, 10]).last).toBeNull();
  });

  it("places hour ticks every 6 h on the local clock", () => {
    const end = new Date(2026, 9, 28, 22, 0).getTime();
    const ticks = hourTicks({ end: new Date(end).toISOString(), step_s: 900, count: 96 });
    expect(ticks.map((t) => new Date(t.at).getHours())).toEqual([0, 6, 12, 18]);
    expect(ticks[0]!.x).toBeCloseTo(2 / 24);
    expect(ticks[3]!.x).toBeCloseTo(20 / 24);
  });
});
