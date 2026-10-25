import { request } from "./client";

// Lucina's Pokémon run: /v1/game/* (secretary-server → Lucina → the game server on nezopt).

export interface GameMon {
  name: string;
  species?: string;
  level: number;
  hp: number;
  max_hp: number;
}

export interface GameState {
  running: boolean;
  map_name: string;
  mode?: string;
  badges: number;
  objective?: string;
  events: string[];
  party: GameMon[];
}

export const gameApi = {
  state: () => request<GameState>("GET", "/v1/game/state"),
  /** Start (`true`) or pause the run. */
  setRunning: (running: boolean) => request<unknown>("POST", "/v1/game/control", { body: { running } }),
  /** The current screen (PNG), with a cache-buster so the browser fetches it again. */
  screenshotUrl: (tick: number) => `/v1/game/screenshot?t=${tick}`,
};

/** "ROCKET_HIDEOUT_B4F" → "Rocket Hideout B4F". */
export function placeName(map: string): string {
  return map
    .toLowerCase()
    .split("_")
    .map((w) => (/^b?\d+f$/.test(w) ? w.toUpperCase() : w.charAt(0).toUpperCase() + w.slice(1)))
    .join(" ");
}

/** Her own notes, newest first, without the server's housekeeping lines. */
export function herNotes(events: string[]): string[] {
  return events.filter((e) => !e.startsWith("Game server restarted") && !e.startsWith("Lucina is playing")).reverse();
}
