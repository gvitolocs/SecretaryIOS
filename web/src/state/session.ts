import { createSignal } from "solid-js";
import { setUnauthorizedHandler, toApiError } from "../api/client";
import { api } from "../api/secretary";
import { clearChat } from "./chat";
import { resetShopping } from "./shopping";

export type SessionState =
  | { status: "loading" }
  | { status: "error"; message: string }
  | { status: "anon" }
  | { status: "authed"; deviceName: string | null };

const [session, setSession] = createSignal<SessionState>({ status: "loading" });
export { session };

function forgetLocalData(): void {
  clearChat();
  resetShopping();
}

export async function checkSession(): Promise<void> {
  try {
    const s = await api.getSession();
    if (s?.authenticated === true) {
      setSession({ status: "authed", deviceName: typeof s.deviceName === "string" ? s.deviceName : null });
    } else {
      setSession({ status: "anon" });
    }
  } catch (e) {
    const err = toApiError(e);
    if (err.status === 401) setSession({ status: "anon" });
    else setSession({ status: "error", message: err.message });
  }
}

/** Throws ApiError on failure (401 bad key, 429 rate limited, …). */
export async function signIn(token: string): Promise<void> {
  await api.login(token);
  await checkSession();
  const now = session();
  if (now.status === "anon") {
    // Cookie did not stick (e.g. blocked cookies).
    throw toApiError(new Error("Signed in, but the session didn't stick. Are cookies allowed?"));
  }
}

export async function signOut(): Promise<void> {
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
