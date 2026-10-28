import { createSignal } from "solid-js";

// "Show adult plugins" (owner, 2026-10-28): a per-device switch on the Plugins page. Adult plugins
// (NClient, Pixiv's R-18 modes) are owner-only on the server; this only hides them on this device.
// Default on: the owner is the only account.

const KEY = "secretary.showAdultPlugins";

function read(): boolean {
  try {
    return localStorage.getItem(KEY) !== "0";
  } catch {
    return true;
  }
}

const [showAdult, setSignal] = createSignal(read());

export { showAdult };

export function setShowAdult(on: boolean): void {
  try {
    localStorage.setItem(KEY, on ? "1" : "0");
  } catch {
    // Private mode: the choice lasts for this page only.
  }
  setSignal(on);
  // Secretary Code's rail hides NClient right away (it also rereads the key on every rail click).
  window.secretaryShell?.setAdultPlugins?.(on);
}
