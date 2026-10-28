// Owner approvals (contract: approvals-contract.md, server: approvals.rs). This device is an approver: it lists the
// requests it may decide, answers them, and picks the "main device" that gets them first. A web browser has no
// Face ID key, so it can approve only requests that are not sensitive; denying never needs proof.

import { request } from "./client";

export type ApprovalRisk = "low" | "medium" | "high";
export type ApprovalStatus = "pending" | "approved" | "denied" | "expired";

export interface ApprovalRequester {
  kind: string;
  name: string;
  detail?: string;
}

export interface Approval {
  id: string;
  requester: ApprovalRequester;
  action: string;
  summary: string;
  /** The exact thing that will run or be revealed; shown in monospace. */
  details: string;
  risk: ApprovalRisk;
  sensitive: boolean;
  /** Echoed back when deciding, so an answer only counts for what was shown. */
  payloadHash: string;
  createdAt: string;
  expiresAt: string;
  escalated: boolean;
  youAreMain: boolean;
  /** `GET /v1/approvals/{id}` also returns answered ones; the list is pending only. */
  status: ApprovalStatus;
  decidedAt?: string;
  decidedBy?: string;
}

export type Decision = "approve" | "deny";

export interface ApprovalDevice {
  id: string;
  name: string;
  isMain: boolean;
  hasBiometricKey: boolean;
  lastSeen?: string;
}

export interface ApprovalSettings {
  mainDeviceId: string | null;
  escalateAfterSecs: number;
  devices: ApprovalDevice[];
  thisDeviceId: string;
}

const STATUSES: readonly string[] = ["pending", "approved", "denied", "expired"];

function str(v: unknown, fallback = ""): string {
  return typeof v === "string" ? v : fallback;
}

/** Network data is untrusted: keep well-formed items only. */
export function parseApproval(v: unknown): Approval | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  if (typeof o.id !== "string" || !o.id || typeof o.payloadHash !== "string") return null;
  // The requester is {kind, name, detail?}; tolerate a bare name.
  const r = o.requester;
  const requester: ApprovalRequester =
    r && typeof r === "object"
      ? {
          kind: str((r as Record<string, unknown>).kind),
          name: str((r as Record<string, unknown>).name),
          ...(typeof (r as Record<string, unknown>).detail === "string" && (r as Record<string, unknown>).detail
            ? { detail: (r as Record<string, unknown>).detail as string }
            : {}),
        }
      : { kind: "", name: str(r) };
  const risk = o.risk === "low" || o.risk === "medium" || o.risk === "high" ? o.risk : "medium";
  const a: Approval = {
    id: o.id,
    requester,
    action: str(o.action),
    summary: str(o.summary),
    details: str(o.details),
    risk,
    sensitive: o.sensitive === true,
    payloadHash: o.payloadHash,
    createdAt: str(o.createdAt),
    expiresAt: str(o.expiresAt),
    escalated: o.escalated === true,
    youAreMain: o.youAreMain === true,
    status: typeof o.status === "string" && STATUSES.includes(o.status) ? (o.status as ApprovalStatus) : "pending",
  };
  if (typeof o.decidedAt === "string") a.decidedAt = o.decidedAt;
  if (typeof o.decidedBy === "string") a.decidedBy = o.decidedBy;
  return a;
}

/** The list is an array; tolerate `{items: [...]}`. */
export function parseApprovals(v: unknown): Approval[] {
  const list = Array.isArray(v) ? v : v && typeof v === "object" && Array.isArray((v as { items?: unknown }).items) ? (v as { items: unknown[] }).items : [];
  return list.map(parseApproval).filter((a): a is Approval => a !== null);
}

export function parseSettings(v: unknown): ApprovalSettings | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  if (!Array.isArray(o.devices)) return null;
  const devices: ApprovalDevice[] = [];
  for (const d of o.devices) {
    if (!d || typeof d !== "object") continue;
    const x = d as Record<string, unknown>;
    if (typeof x.id !== "string" || !x.id) continue;
    devices.push({
      id: x.id,
      name: str(x.name, x.id),
      isMain: x.isMain === true,
      hasBiometricKey: x.hasBiometricKey === true,
      ...(typeof x.lastSeen === "string" ? { lastSeen: x.lastSeen } : {}),
    });
  }
  return {
    mainDeviceId: typeof o.mainDeviceId === "string" && o.mainDeviceId ? o.mainDeviceId : null,
    escalateAfterSecs: typeof o.escalateAfterSecs === "number" ? o.escalateAfterSecs : 30,
    devices,
    thisDeviceId: str(o.thisDeviceId),
  };
}

export const approvalsApi = {
  /** The requests this device may decide now, newest first. */
  async listPending(): Promise<Approval[]> {
    return parseApprovals(await request<unknown>("GET", "/v1/approvals?status=pending"));
  },
  /** One request, answered or expired ones too. */
  async get(id: string): Promise<Approval | null> {
    return parseApproval(await request<unknown>("GET", `/v1/approvals/${encodeURIComponent(id)}`));
  },
  decide: (id: string, decision: Decision, payloadHash: string) =>
    request<{ status: "approved" | "denied" }>("POST", `/v1/approvals/${encodeURIComponent(id)}/decide`, {
      body: { decision, payloadHash },
    }),
  async settings(): Promise<ApprovalSettings | null> {
    return parseSettings(await request<unknown>("GET", "/v1/approvals/settings"));
  },
  setMainDevice: (mainDeviceId: string | null) =>
    request<unknown>("PUT", "/v1/approvals/settings", { body: { mainDeviceId } }),
};
