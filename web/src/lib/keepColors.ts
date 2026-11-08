// Keep's colour palette (the owner compares side by side with keep.google.com), per theme.

import { KEEP_COLORS, type KeepColor } from "../api/keep";

export type KeepTheme = "dark" | "light";

export const PALETTE: Record<KeepColor, { dark: string; light: string }> = {
  default: { dark: "transparent", light: "#ffffff" },
  red: { dark: "#77172e", light: "#faafa8" },
  orange: { dark: "#692b17", light: "#f39f76" },
  yellow: { dark: "#7c4a03", light: "#fff8b8" },
  green: { dark: "#264d3b", light: "#e2f6d3" },
  teal: { dark: "#0c625d", light: "#b4ddd3" },
  blue: { dark: "#256377", light: "#d4e4ed" },
  cerulean: { dark: "#284255", light: "#aeccdc" },
  purple: { dark: "#472e5b", light: "#d3bfdb" },
  pink: { dark: "#6c394f", light: "#f6e2dd" },
  brown: { dark: "#4b443a", light: "#e9e3d4" },
  gray: { dark: "#232427", light: "#efeff1" },
};

export const DEFAULT_BORDER: Record<KeepTheme, string> = { dark: "#5f6368", light: "#e0e0e0" };
export const PAGE_BG: Record<KeepTheme, string> = { dark: "#202124", light: "#ffffff" };

export function isKeepColor(v: unknown): v is KeepColor {
  return typeof v === "string" && (KEEP_COLORS as readonly string[]).includes(v);
}

/** The card background for a note colour; "default" is transparent in the dark theme (the page shows through). */
export function cardBackground(color: KeepColor, theme: KeepTheme): string {
  return (PALETTE[color] ?? PALETTE.default)[theme];
}

/** Coloured cards have no visible border; the default card has Keep's grey one. */
export function cardBorder(color: KeepColor, theme: KeepTheme): string {
  return color === "default" ? DEFAULT_BORDER[theme] : "transparent";
}

/** A swatch's fill in the palette popover: the default swatch shows the page colour, outlined. */
export function swatchFill(color: KeepColor, theme: KeepTheme): string {
  return color === "default" ? PAGE_BG[theme] : PALETTE[color][theme];
}
