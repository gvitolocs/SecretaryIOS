// Secretary Code (the Mac app) can approve sensitive requests with Touch ID: its preload exposes
// `window.secretaryApprover`. The main process does the network calls and the Touch ID prompt itself, so the page
// never sees a key or a signature; it only starts the action and shows the result. In a browser and on Android the
// bridge is absent, and everything that uses it stays hidden.

/** Why an action did not go through. "cancelled" is the owner dismissing Touch ID: show nothing. */
export type ApproverError =
  | "cancelled"
  | "unavailable"
  | "needsSetup"
  | "decided"
  | "expired"
  | "notPending"
  | "changed"
  | "notMain"
  | "server"
  | "network";

export type ApproverFailure = { ok: false; error: ApproverError; message?: string };

export interface ApproverStatus {
  /** This Mac has a Secure Enclave and Touch ID or a login password. */
  available: boolean;
  /** A key exists locally. */
  hasKey: boolean;
  /** The server's id for it, once registered. */
  keyId?: string;
}

export interface SecretaryApprover {
  version: 1;
  status(): Promise<ApproverStatus>;
  /**
   * Creates the key (Touch ID prompt) and registers it. "active" for the first approver device ever; otherwise
   * "pending_approval": the owner approves "Add <Mac> as an approver device" on his iPhone.
   */
  setup(): Promise<{ ok: true; status: "active" | "pending_approval"; approvalId?: string } | ApproverFailure>;
  /** Approves request `approvalId` with Touch ID (fetch, prompt, sign and post all happen in the main process). */
  approve(approvalId: string): Promise<{ ok: true; status: "approved" } | ApproverFailure>;
  /** Revokes another device's approver key (Touch ID prompt). */
  revoke(deviceId: string): Promise<{ ok: true; removed: boolean } | ApproverFailure>;
  /** Forgets this Mac's key, locally and on the server. */
  remove(): Promise<{ ok: true } | ApproverFailure>;
}

declare global {
  interface Window {
    secretaryApprover?: SecretaryApprover;
  }
}

/** The Touch ID bridge, when this page runs inside Secretary Code and the app offers it. */
export function desktopApprover(): SecretaryApprover | undefined {
  if (typeof window === "undefined") return undefined;
  const a = window.secretaryApprover;
  if (!a || a.version !== 1) return undefined;
  for (const fn of ["status", "setup", "approve", "revoke", "remove"] as const) {
    if (typeof a[fn] !== "function") return undefined;
  }
  return a;
}
