import { describe, expect, it } from "vitest";
import { galleryItem } from "../test/galleryFixtures";
import {
  daysLeft,
  deviceNames,
  formatExposure,
  formatNumber,
  formatTaken,
  joinNames,
  megapixels,
  nextRotation,
  normalizeOffset,
  offsetMinutes,
  placeLine,
} from "./galleryInfo";

describe("galleryInfo helpers", () => {
  it("reads time zone offsets", () => {
    expect(offsetMinutes("+02:00")).toBe(120);
    expect(offsetMinutes("-0530")).toBe(-330);
    expect(offsetMinutes("Z")).toBeNull();
    expect(offsetMinutes(null)).toBeNull();
    expect(normalizeOffset("-0530")).toBe("-05:30");
    expect(normalizeOffset("nope")).toBeNull();
  });

  it("shows the date as the wall clock read where the picture was taken", () => {
    expect(formatTaken("2026-03-04T12:03:00Z", "+02:00", "en-GB")).toContain("14:03");
    expect(formatTaken("2026-03-04T12:03:00Z", "-05:00", "en-GB")).toContain("07:03");
    expect(formatTaken("2026-03-04T12:03:00Z", "+02:00", "en-GB")).toContain("2026");
    // Without an offset it is this device's time zone, still a date.
    expect(formatTaken("2026-03-04T12:03:00Z", null, "en-GB")).toContain("2026");
    expect(formatTaken(null, "+02:00", "en")).toBe("");
    expect(formatTaken("not a date", null, "en")).toBe("");
  });

  it("formats camera settings", () => {
    expect(formatExposure(0.004)).toBe("1/250");
    expect(formatExposure(0.5)).toBe("1/2");
    expect(formatExposure(2)).toBe("2");
    expect(formatExposure(2.54)).toBe("2.5");
    expect(formatExposure(0)).toBe("");
    expect(formatExposure(null)).toBe("");
    expect(formatNumber(1.8)).toBe("1.8");
    expect(formatNumber(4, 1)).toBe("4");
    expect(formatNumber(6.9, 0)).toBe("7");
    expect(formatNumber(null)).toBe("");
    expect(megapixels(4000, 3000)).toBe("12");
    expect(megapixels(100, 100)).toBe("");
    expect(megapixels(null, 5)).toBe("");
  });

  it("turns a quarter at a time and wraps", () => {
    expect([0, 90, 180, 270].map((r) => nextRotation(r as 0 | 90 | 180 | 270))).toEqual([90, 180, 270, 0]);
  });

  it("lists the devices behind items once each", () => {
    const items = [
      galleryItem("a"),
      galleryItem("b"),
      galleryItem("c", { device: "dev-2", deviceName: "Test Mac" }),
      galleryItem("d", { device: "dev-3", deviceName: null }),
      galleryItem("e", { device: null, deviceName: null, source: "web-chat" }),
    ];
    expect(deviceNames(items)).toEqual(["Test iPhone", "Test Mac", "dev-3"]);
    expect(deviceNames([items[4]!])).toEqual([]);
    expect(joinNames(["iPhone", "Mac"], "en")).toBe("iPhone and Mac");
    expect(joinNames(["iPhone"], "en")).toBe("iPhone");
  });

  it("counts the days left in the Trash", () => {
    const now = Date.parse("2026-10-01T12:00:00Z");
    expect(daysLeft("2026-10-04T12:00:00Z", now)).toBe(3);
    expect(daysLeft("2026-10-04T13:00:00Z", now)).toBe(4);
    expect(daysLeft("2026-09-01T00:00:00Z", now)).toBe(0);
    expect(daysLeft(null, now)).toBeNull();
    expect(daysLeft("garbage", now)).toBeNull();
  });

  it("writes the place on one line without repeats", () => {
    expect(placeLine({ name: "Aarhus", admin1: "Central Denmark", country: "Denmark", lat: 1, lon: 2 })).toBe("Aarhus, Central Denmark, Denmark");
    expect(placeLine({ name: "Monaco", admin1: "Monaco", country: "Monaco", lat: 1, lon: 2 })).toBe("Monaco");
    expect(placeLine(null)).toBe("");
  });
});
