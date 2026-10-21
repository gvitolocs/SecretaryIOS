import { createSignal } from "solid-js";
import { setLucinaOfflineHandler, type ApiError } from "../api/client";

// Shared "Lucina is offline" notice, set by any request that comes back with
// lucina_unreachable / lucina_timeout and cleared by the next success.
const [lucinaNotice, setLucinaNotice] = createSignal<string | null>(null);

export { lucinaNotice };

export function markLucinaOffline(err: ApiError): void {
  setLucinaNotice(
    err.code === "lucina_timeout"
      ? "Lucina took too long to answer. She may be busy or offline; try again in a moment."
      : "Lucina is offline right now. Your messages stay here, so you can retry once she's back.",
  );
}

export function markServerOffline(): void {
  setLucinaNotice("Secretary's server isn't reachable right now. Your last change wasn't saved.");
}

export function markLucinaOnline(): void {
  setLucinaNotice(null);
}

setLucinaOfflineHandler(markLucinaOffline);
