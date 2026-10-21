import { describe, expect, it } from "vitest";
import { formatCountdown, formatDistance, formatDuration, formatWeight, weightValue } from "./units";

describe("formatting", () => {
  it("formats weights in the chosen unit", () => {
    expect(formatWeight(80500, "kg")).toBe("80.5");
    expect(formatWeight(80000, "kg")).toBe("80");
    expect(formatWeight(45359, "lb")).toBe("100");
    expect(weightValue(45359.237, "lb")).toBeCloseTo(100, 6);
  });

  it("formats durations, countdowns and distances", () => {
    expect(formatDuration(245)).toBe("4m 05s");
    expect(formatDuration(3720)).toBe("1h 02m");
    expect(formatCountdown(90)).toBe("1:30");
    expect(formatCountdown(5)).toBe("0:05");
    expect(formatCountdown(-3)).toBe("0:00");
    expect(formatDistance(1200)).toBe("1.2 km");
    expect(formatDistance(800)).toBe("800 m");
  });
});
