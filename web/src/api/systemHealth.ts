import { request } from "./client";

// Server health (Plugins → Server health): GET /v1/system/health. Each host's sentinel posts a
// snapshot every 5 minutes; the server keeps the latest per host and the state changes between them.

export type HealthState = "ok" | "warn" | "crit";
export type CheckGroup = "Lucina" | "Pokoin" | "CardRails" | "AI" | "Homelab" | "Other";

export const GROUPS: CheckGroup[] = ["Lucina", "Pokoin", "CardRails", "AI", "Homelab", "Other"];

export interface HealthCheck {
  id: string;
  kind: "unit" | "timer" | "container" | "k3s" | "forward" | "log" | "disk" | "deadman";
  name: string;
  group: CheckGroup;
  state: HealthState;
  detail: string;
  since: string;
  restarts_1h: number;
}

export interface HealthSnapshot {
  host: string;
  at: string;
  overall: HealthState;
  metrics: {
    disks: { mount: string; used_pct: number; free_gb: number }[];
    cpu_temp_c: number | null;
    load: [number, number, number];
    mem_used_pct: number;
    swap_used_pct?: number | null;
  };
  checks: HealthCheck[];
}

export interface HostHealth {
  host: string;
  received_at: string;
  snapshot: HealthSnapshot;
  /** Nothing received for more than 15 minutes. */
  stale: boolean;
}

export interface HealthEvent {
  at: string;
  host: string;
  check_id: string;
  name: string;
  from: HealthState;
  to: HealthState;
  detail: string;
}

export interface SystemHealth {
  hosts: HostHealth[];
  /** Newest first. */
  events: HealthEvent[];
}

export const systemHealthApi = {
  get: () => request<SystemHealth>("GET", "/v1/system/health"),
};

const RANK: Record<HealthState, number> = { crit: 0, warn: 1, ok: 2 };

/** Checks that are not ok, crit first, then the oldest problem first. */
export function needsAttention(checks: HealthCheck[]): HealthCheck[] {
  return checks
    .filter((c) => c.state !== "ok")
    .sort((a, b) => RANK[a.state] - RANK[b.state] || Date.parse(a.since) - Date.parse(b.since) || a.name.localeCompare(b.name));
}

/** The ok checks by group, in GROUPS order, each sorted by name; empty groups left out. */
export function healthyByGroup(checks: HealthCheck[]): { group: CheckGroup; checks: HealthCheck[] }[] {
  return GROUPS.map((group) => ({
    group,
    checks: checks.filter((c) => c.state === "ok" && (c.group === group || (group === "Other" && !GROUPS.includes(c.group)))).sort((a, b) => a.name.localeCompare(b.name)),
  })).filter((g) => g.checks.length > 0);
}
