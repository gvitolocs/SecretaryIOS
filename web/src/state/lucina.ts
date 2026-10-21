import { createSignal } from "solid-js";
import { setLucinaOfflineHandler, type ApiError } from "../api/client";
import { t, type MessageKey } from "../i18n";

// Shared "Lucina is offline" notice, set by any request that comes back with
// lucina_unreachable / lucina_timeout and cleared by the next success.
const [notice, setNotice] = createSignal<MessageKey | null>(null);

/** The notice text in the current language, or null. */
export function lucinaNotice(): string | null {
  const key = notice();
  return key ? t(key) : null;
}

export function markLucinaOffline(err: ApiError): void {
  setNotice(err.code === "lucina_timeout" ? "shell.noticeTimeout" : "shell.noticeOffline");
}

export function markServerOffline(): void {
  setNotice("shell.noticeServer");
}

export function markLucinaOnline(): void {
  setNotice(null);
}

setLucinaOfflineHandler(markLucinaOffline);
