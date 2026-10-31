import { createRoot, createSignal } from "solid-js";
import { readString, writeString } from "../lib/storage";

// Appearance preferences, per browser: the colour theme and the quality of
// Lucina's 3D stage. "system" follows prefers-color-scheme; "dark"/"light"
// set <html data-theme>, which the tokens in styles/tokens.css read.

export type ThemeSetting = "system" | "dark" | "light";
export type StageQualitySetting = "auto" | "low" | "medium" | "high";

export const THEME_KEY = "secretary.theme";
export const QUALITY_KEY = "secretary.stage.quality";
export const THEMES: readonly ThemeSetting[] = ["system", "dark", "light"];
export const QUALITIES: readonly StageQualitySetting[] = ["auto", "low", "medium", "high"];

function readTheme(): ThemeSetting {
  const v = readString("local", THEME_KEY);
  return (THEMES as readonly string[]).includes(v ?? "") ? (v as ThemeSetting) : "system";
}

function readQuality(): StageQualitySetting {
  const v = readString("local", QUALITY_KEY);
  return (QUALITIES as readonly string[]).includes(v ?? "") ? (v as StageQualitySetting) : "auto";
}

function prefersLight(): MediaQueryList | null {
  try {
    return typeof matchMedia === "function" ? matchMedia("(prefers-color-scheme: light)") : null;
  } catch {
    return null;
  }
}

const state = createRoot(() => {
  const [theme, setThemeSignal] = createSignal<ThemeSetting>(readTheme());
  const [quality, setQualitySignal] = createSignal<StageQualitySetting>(readQuality());
  const mql = prefersLight();
  const [systemLight, setSystemLight] = createSignal(!!mql?.matches);
  mql?.addEventListener?.("change", (e) => setSystemLight(e.matches));
  return { theme, setThemeSignal, quality, setQualitySignal, systemLight };
});

function applyTheme(setting: ThemeSetting): void {
  if (typeof document === "undefined") return;
  const el = document.documentElement;
  if (setting === "system") delete el.dataset.theme;
  else el.dataset.theme = setting;
}

applyTheme(state.theme());

export const themeSetting = state.theme;
export const stageQuality = state.quality;

/** The theme actually shown. */
export function effectiveTheme(): "dark" | "light" {
  const s = state.theme();
  return s === "system" ? (state.systemLight() ? "light" : "dark") : s;
}

export function setThemeSetting(setting: ThemeSetting): void {
  state.setThemeSignal(setting);
  writeString("local", THEME_KEY, setting);
  applyTheme(setting);
}

export function setStageQuality(q: StageQualitySetting): void {
  state.setQualitySignal(q);
  writeString("local", QUALITY_KEY, q);
}
