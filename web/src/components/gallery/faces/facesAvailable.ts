import { getFaceSettings } from "../../../api/gallery";

// Whether face recognition is on, asked once a minute at most (every photo viewer asks, and most never need the answer).

let cache: { at: number; enabled: Promise<boolean> } | null = null;

/** True when the server has face recognition turned on; false when it is off, missing, or unreachable. */
export function facesAvailable(): Promise<boolean> {
  if (cache && Date.now() - cache.at < 60_000) return cache.enabled;
  const enabled = getFaceSettings().then(
    (s) => s.enabled,
    () => false,
  );
  cache = { at: Date.now(), enabled };
  return enabled;
}

/** The People page turned recognition on or off, or a test starts over: ask again next time. */
export function resetFacesAvailable(): void {
  cache = null;
}
