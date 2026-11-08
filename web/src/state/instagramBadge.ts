import { createSignal } from "solid-js";
import { instagramApi, type IgBadges, type IgWatch, type IgWatchSettings } from "../api/instagram";

// The red dots of the Instagram plugin: new stories and posts of the accounts the owner watches
// (GET /v1/instagram/badges). Polled while the app is open; the total also goes to the desktop rail
// (window.secretaryShell.setBadge, Secretary Code) and the Plugins card. An account's own count
// clears when the owner opens it (POST /v1/instagram/watch/{user}/opened).

const POLL_MS = 120_000;
const NONE: IgBadges = { total: 0, accounts: {} };

const [badges, setBadges] = createSignal<IgBadges>(NONE);
export { badges as instagramBadges };

const [watched, setWatched] = createSignal<IgWatch[]>([]);
/** The accounts the owner watches (GET /v1/instagram/watch). */
export { watched as instagramWatched };

export async function refreshInstagramWatched(): Promise<void> {
  try {
    setWatched(await instagramApi.watchList());
  } catch {
    // Offline or not the owner: keep what we had.
  }
}

const [watchSettings, setWatchSettings] = createSignal<IgWatchSettings | null>(null);
/** How often watched accounts are checked (GET /v1/instagram/watch-settings); null until loaded. */
export { watchSettings as instagramWatchSettings, setWatchSettings as setInstagramWatchSettings };

let settingsAsked = false;
/** Load the settings once per session (again after a failure). */
export async function loadInstagramWatchSettings(): Promise<void> {
  if (settingsAsked) return;
  settingsAsked = true;
  try {
    setWatchSettings(await instagramApi.watchSettings());
  } catch {
    settingsAsked = false;
  }
}

function apply(next: IgBadges): void {
  setBadges(next);
  try {
    window.secretaryShell?.setBadge?.("instagram", next.total);
  } catch {
    // The rail is not there or refused: the dots in the page are enough.
  }
}

/** Use badges the server just sent (after opening an account). */
export function setInstagramBadges(next: IgBadges): void {
  apply(next);
}

export async function refreshInstagramBadges(): Promise<void> {
  try {
    apply(await instagramApi.badges());
  } catch {
    // Not the owner (403), the service is off (404/502) or offline: no dots.
  }
}

export function resetInstagramBadges(): void {
  setWatched([]);
  setWatchSettings(null);
  settingsAsked = false;
  apply(NONE);
}

/** Poll while the app is open (on start, every 2 minutes, and when the window is focused again). */
export function startInstagramBadges(): () => void {
  void refreshInstagramBadges();
  const timer = setInterval(() => void refreshInstagramBadges(), POLL_MS);
  const onFocus = () => void refreshInstagramBadges();
  window.addEventListener("focus", onFocus);
  return () => {
    clearInterval(timer);
    window.removeEventListener("focus", onFocus);
  };
}
