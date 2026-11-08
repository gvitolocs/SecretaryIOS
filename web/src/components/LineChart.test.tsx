import { render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { LineChart, turningPoints } from "./LineChart";

// The shape of a real weight history: a long fall, a low, a climb, a noisy drop, a bump, today.
const HISTORY = [
  [0, 83.2], [2, 83.8], [30, 78.6], [33, 76.8], [37, 75.0], [39, 73.2], [41, 72.2], [47, 70.0], [68, 81.5],
  [68.3, 80.5], [68.6, 80.2], [69, 79.7], [70, 79.0], [71, 78.6], [71.5, 77.3], [72, 76.3], [76, 78.7], [88, 77.2],
  [90, 77.9], [90.2, 76.8], [92, 76.2], [100, 78.2],
].map(([x, y]) => ({ x: x!, y: y! }));

const values = (max: number) => turningPoints(HISTORY, max).map((m) => HISTORY[m.index]!.y);

describe("turningPoints", () => {
  it("finds the low, the high and today", () => {
    const got = values(7);
    expect(got).toContain(70.0);
    expect(got).toContain(81.5);
    expect(got).toContain(78.2);
    expect(got.length).toBeLessThanOrEqual(7);
  });

  it("keeps the marks apart where the line is noisy", () => {
    const marks = turningPoints(HISTORY, 7).map((m) => HISTORY[m.index]!.x);
    for (let i = 1; i < marks.length; i++) expect(marks[i]! - marks[i - 1]!).toBeGreaterThanOrEqual(10);
  });

  it("puts a peak's number above the line and a low's below it", () => {
    const at = (y: number) => turningPoints(HISTORY, 7).find((m) => HISTORY[m.index]!.y === y)!;
    expect(at(81.5).above).toBe(true);
    expect(at(70.0).above).toBe(false);
  });

  it("marks nothing on a flat or too short line", () => {
    expect(turningPoints([{ x: 0, y: 5 }, { x: 1, y: 5 }, { x: 2, y: 5 }], 7)).toEqual([]);
    expect(turningPoints([{ x: 0, y: 5 }], 7)).toEqual([]);
  });

  it("marks only today on a straight line", () => {
    const straight = [{ x: 0, y: 1 }, { x: 1, y: 2 }, { x: 2, y: 3 }];
    expect(turningPoints(straight, 1).map((m) => m.index)).toEqual([2]);
  });
});

describe("LineChart marks", () => {
  const chart = (marks?: number) => render(() => <LineChart ariaLabel="w" series={[{ key: "w", points: HISTORY }]} marks={marks} />);

  it("draws small numbers on the line when asked", () => {
    const { container } = chart(7);
    const texts = [...container.querySelectorAll(".chart-mark")].map((e) => e.textContent);
    expect(texts).toContain("70");
    expect(texts).toContain("81.5");
    expect(texts).toContain("78.2");
  });

  it("leaves the peak next to the axis to the axis's own number", () => {
    const { container } = chart(7);
    expect([...container.querySelectorAll(".chart-mark")].map((e) => e.textContent)).not.toContain("83.8");
    expect(container.querySelector(".chart-y--max")?.textContent).toBe("83.8");
  });

  it("draws none by default", () => {
    expect(chart().container.querySelector(".chart-marks")).toBeNull();
  });
});
