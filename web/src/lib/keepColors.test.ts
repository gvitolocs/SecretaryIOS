import { describe, expect, it } from "vitest";
import { KEEP_COLORS } from "../api/keep";
import { cardBackground, cardBorder, DEFAULT_BORDER, isKeepColor, PALETTE, swatchFill } from "./keepColors";

describe("Keep's palette", () => {
  it("maps every colour to the dark values", () => {
    const dark = { red: "#77172e", orange: "#692b17", yellow: "#7c4a03", green: "#264d3b", teal: "#0c625d", blue: "#256377", cerulean: "#284255", purple: "#472e5b", pink: "#6c394f", brown: "#4b443a", gray: "#232427" };
    for (const [c, hex] of Object.entries(dark)) expect(cardBackground(c as never, "dark")).toBe(hex);
  });

  it("maps every colour to the light values", () => {
    const light = { red: "#faafa8", orange: "#f39f76", yellow: "#fff8b8", green: "#e2f6d3", teal: "#b4ddd3", blue: "#d4e4ed", cerulean: "#aeccdc", purple: "#d3bfdb", pink: "#f6e2dd", brown: "#e9e3d4", gray: "#efeff1" };
    for (const [c, hex] of Object.entries(light)) expect(cardBackground(c as never, "light")).toBe(hex);
  });

  it("makes the default card transparent in the dark theme and white in the light one", () => {
    expect(cardBackground("default", "dark")).toBe("transparent");
    expect(cardBackground("default", "light")).toBe("#ffffff");
  });

  it("outlines only the default card", () => {
    expect(cardBorder("default", "dark")).toBe(DEFAULT_BORDER.dark);
    expect(cardBorder("default", "dark")).toBe("#5f6368");
    expect(cardBorder("default", "light")).toBe("#e0e0e0");
    expect(cardBorder("red", "dark")).toBe("transparent");
  });

  it("covers exactly the colours the contract names", () => {
    expect(Object.keys(PALETTE).sort()).toEqual([...KEEP_COLORS].sort());
    expect(isKeepColor("teal")).toBe(true);
    expect(isKeepColor("magenta")).toBe(false);
  });

  it("shows the default swatch as the page colour", () => {
    expect(swatchFill("default", "dark")).toBe("#202124");
    expect(swatchFill("pink", "light")).toBe("#f6e2dd");
  });
});
