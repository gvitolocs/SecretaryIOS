// What Touch ID on this Mac can do right now (Secretary Code only): shared by the approval cards and the account
// section, so setting it up in one place updates the other. Asked again whenever a card opens or the section
// changes it; a failed question counts as "not available", so the card falls back to the iPhone-only note.

import { createSignal } from "solid-js";
import { desktopApprover, type SecretaryApprover, type ApproverStatus } from "../api/desktopApprover";

const [state, setState] = createSignal<{ bridge: SecretaryApprover; status: ApproverStatus | null } | null>(null);

/** This Mac's Touch ID status, or null while unknown (also when the bridge is absent). */
export function macApproverStatus(): ApproverStatus | null {
  const s = state();
  // A different bridge (the page was reloaded into another host, or a test swapped it) invalidates the answer.
  return s && s.bridge === desktopApprover() ? s.status : null;
}

/** Ask the desktop app again. Keeps the last answer on screen meanwhile. */
export async function refreshMacApprover(): Promise<ApproverStatus | null> {
  const bridge = desktopApprover();
  if (!bridge) {
    setState(null);
    return null;
  }
  let status: ApproverStatus | null;
  try {
    const r = await bridge.status();
    status = { available: r.available === true, hasKey: r.hasKey === true, ...(typeof r.keyId === "string" ? { keyId: r.keyId } : {}) };
  } catch {
    status = { available: false, hasKey: false };
  }
  // Only a current bridge may write: the answer of a replaced one is dropped.
  if (bridge === desktopApprover()) setState({ bridge, status });
  return status;
}

/** This Mac can approve sensitive requests with Touch ID now. */
export function macCanApprove(): boolean {
  const s = macApproverStatus();
  return !!s && s.available && s.hasKey;
}
