import { describe, expect, it } from "vitest";
import { gameApi, herNotes, placeName } from "./game";

describe("game helpers", () => {
  it("names places like the game", () => {
    expect(placeName("ROCKET_HIDEOUT_B4F")).toBe("Rocket Hideout B4F");
    expect(placeName("SILPH_CO_11F")).toBe("Silph Co 11F");
    expect(placeName("PALLET_TOWN")).toBe("Pallet Town");
  });
  it("shows her notes newest first, without housekeeping lines", () => {
    expect(herNotes(["Game server restarted", "Walked to 3F", "Lucina is playing (updated tools)", "Opened the door"])).toEqual([
      "Opened the door",
      "Walked to 3F",
    ]);
  });
  it("busts the screenshot cache", () => {
    expect(gameApi.screenshotUrl(5)).toBe("/v1/game/screenshot?t=5");
  });
});
