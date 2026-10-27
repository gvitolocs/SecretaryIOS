import { createSignal } from "solid-js";
import { setUnauthorizedHandler, toApiError } from "../api/client";
import { api } from "../api/secretary";
import { t } from "../i18n";
import { resetBody } from "./body";
import { clearChat, setChatDevice } from "./chat";
import { resetGym } from "./gym";
import { resetKeep } from "./keep";
import { resetNotifications } from "./notifications";
import { resetShopping } from "./shopping";
import { desktopShell } from "../lib/desktopShell";

export type SessionState =
  | { status: "loading" }
  | { status: "error"; message: string }
  | { status: "anon" }
  | { status: "authed"; deviceName: string | null; deviceId?: string };

const [session, setSession] = createSignal<SessionState>({ status: "loading" });
export { session };

function forgetLocalData(): void {
  clearChat();
  resetShopping();
  resetGym();
  resetBody();
  resetNotifications();
  resetKeep();
}

export async function checkSession(): Promise<void> {
  try {
    const s = await api.getSession();
    if (s?.authenticated === true) {
      setChatDevice(s.deviceId);
      setSession({ status: "authed", deviceId: s.deviceId, deviceName: typeof s.deviceName === "string" ? s.deviceName : null });
    } else {
      forgetLocalData();
      setSession({ status: "anon" });
    }
  } catch (e) {
    const err = toApiError(e);
    if (err.status === 401) { forgetLocalData(); setSession({ status: "anon" }); }
    else setSession({ status: "error", message: err.message });
  }
}

/** Throws ApiError on failure (401 invalid_credentials, 429 rate_limited / account_locked, …). */
export async function signIn(email: string, password: string): Promise<void> {
  await api.login(email, password);
  await checkSession();
  const now = session();
  if (now.status === "anon") {
    // Cookie did not stick (e.g. blocked cookies).
    throw toApiError(new Error(t("shell.cookieNotStuck")));
  }
}

export async function signOut(): Promise<void> {
  // Inside Secretary Code the Mac first forgets its device token, or its shell would sign straight back in.
  try {
    await desktopShell()?.signOut?.();
  } catch {
    // The web session still ends below.
  }
  try {
    await api.logout();
  } catch {
    // Forget the session locally regardless; the cookie expires on its own.
  }
  forgetLocalData();
  setSession({ status: "anon" });
}

setUnauthorizedHandler(() => {
  if (session().status === "authed") {
    forgetLocalData();
    setSession({ status: "anon" });
  }
});

/** Test helper. */
export function setSessionForTests(state: SessionState): void {
  setSession(state);
}
