import { describe, expect, it } from "vitest";
import type { IgItem } from "../api/instagram";
import { formatCount, formatTaken, parseTyped, slideIndexOf, slidesOf } from "./instagram";

const img = (id: string): IgItem => ({ id, kind: "image", url: `/u/${id}`, thumbUrl: `/t/${id}` });
const carousel = (id: string): IgItem => ({
  id,
  kind: "carousel",
  url: "/u/c0",
  thumbUrl: "/t/c0",
  children: [
    { id: `${id}_0`, kind: "image", url: "/u/c0", thumbUrl: "/t/c0" },
    { id: `${id}_1`, kind: "video", url: "/u/c1", thumbUrl: null },
  ],
});

describe("parseTyped", () => {
  it("takes usernames with or without @, any case", () => {
    expect(parseTyped("natgeo")).toEqual({ kind: "user", username: "natgeo" });
    expect(parseTyped("  @NatGeo ")).toEqual({ kind: "user", username: "natgeo" });
    expect(parseTyped("a.b_c9")).toEqual({ kind: "user", username: "a.b_c9" });
  });
  it("takes profile links", () => {
    expect(parseTyped("https://www.instagram.com/natgeo/")).toEqual({ kind: "user", username: "natgeo" });
    expect(parseTyped("instagram.com/NatGeo?igsh=1")).toEqual({ kind: "user", username: "natgeo" });
  });
  it("takes post and reel links, with the owner when the link names one", () => {
    expect(parseTyped("https://www.instagram.com/p/DeXzbFEl5DG/")).toEqual({ kind: "post", url: "https://www.instagram.com/p/DeXzbFEl5DG/", username: null });
    expect(parseTyped("https://instagram.com/natgeo/reel/DeXzbFEl5DG/")).toMatchObject({ kind: "post", username: "natgeo" });
  });
  it("refuses everything else", () => {
    for (const bad of ["", "   ", "a b", "x".repeat(31), "https://evil.test/natgeo", "https://www.instagram.com/explore/", "https://www.instagram.com/", "a/b"]) {
      expect(parseTyped(bad), bad).toBeNull();
    }
  });
});

describe("slides", () => {
  it("makes one slide per carousel frame and keeps the owner item", () => {
    const items = [img("a"), carousel("c"), img("b")];
    const slides = slidesOf(items);
    expect(slides.map((s) => s.key)).toEqual(["a", "c_0", "c_1", "b"]);
    expect(slides[2]).toMatchObject({ kind: "video", frame: 1, frames: 2 });
    expect(slides[2]!.item.id).toBe("c");
    expect(slideIndexOf(items, 2)).toBe(3);
    expect(slideIndexOf(items, 0)).toBe(0);
  });
  it("shows a carousel with no frames as its cover", () => {
    expect(slidesOf([{ ...carousel("c"), children: [] }])).toHaveLength(1);
  });
});

describe("formatting", () => {
  it("shortens counts", () => {
    expect(formatCount(0)).toBe("0");
    expect(formatCount(9999)).toBe("9999");
    expect(formatCount(12_500)).toBe("12.5K");
    expect(formatCount(268_000_000)).toBe("268M");
    expect(formatCount(1_500_000_000)).toBe("1.5B");
    expect(formatCount(Number.NaN)).toBe("0");
  });
  it("formats a date or nothing", () => {
    expect(formatTaken(null)).toBe("");
    expect(formatTaken(1_700_000_000, "en-GB")).toContain("2023");
  });
});
