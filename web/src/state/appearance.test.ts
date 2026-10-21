import { afterEach, describe, expect, it } from "vitest";
import { effectiveTheme, QUALITY_KEY, setStageQuality, setThemeSetting, stageQuality, THEME_KEY, themeSetting } from "./appearance";

describe("appearance", () => {
  afterEach(() => {
    setThemeSetting("system");
    setStageQuality("auto");
  });

  it("sets <html data-theme> and remembers the choice", () => {
    setThemeSetting("light");
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(localStorage.getItem(THEME_KEY)).toBe("light");
    expect(themeSetting()).toBe("light");
    expect(effectiveTheme()).toBe("light");
    setThemeSetting("dark");
    expect(effectiveTheme()).toBe("dark");
    setThemeSetting("system");
    expect(document.documentElement.dataset.theme).toBeUndefined();
  });

  it("stores the stage quality", () => {
    setStageQuality("low");
    expect(stageQuality()).toBe("low");
    expect(localStorage.getItem(QUALITY_KEY)).toBe("low");
  });
});
