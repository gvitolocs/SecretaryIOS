import { request } from "./client";

// The Homelab service catalog through Lucina Agents (/v1/agents/catalog → lucina-agentd /catalog → the `catalog` CLI on
// nezopt, repo ~/Projects/Homelab/catalog). Read-only. JSON keys are snake_case (the CLI's), except the agents and
// workspaces agentd adds to each project of the tree (camelCase, as the rest of /v1/agents).

const B = "/v1/agents/catalog";
const enc = encodeURIComponent;

/** Own health: ok, idle (on-demand, not running), unknown, warn, down. Effective adds broken/degraded (a dependency). */
export type Level = "ok" | "idle" | "unknown" | "warn" | "down";
export type Effective = Level | "broken" | "degraded";

export interface Finding {
  level: Level;
  what: string;
  detail: string;
}

export interface ComponentStatus {
  level: Level;
  observed: Level;
  since: string;
  effective: Effective;
  impacted_by: string[];
  findings: Finding[];
  failures_24h: number;
}

export interface Deploy {
  runtime: string;
  commit: string;
  target: string;
  recorded: string;
  subject: string;
}

export interface CatalogComponent {
  id: string;
  name?: string | null;
  kind: string;
  group: string;
  project: string;
  host?: string | null;
  description?: string;
  repo?: string | null;
  path?: string | null;
  branch?: string | null;
  units?: string[];
  containers?: string[];
  k8s?: string[];
  ports?: number[];
  serves?: string[];
  depends_on?: string[];
  soft_depends_on?: string[];
  depends_on_any?: string[][];
  logs?: string | null;
  restart?: string | null;
  rollback?: string | null;
  runbook?: string;
  criticality: "low" | "medium" | "high" | "critical";
  optional?: boolean;
  status?: ComponentStatus | null;
  dependents?: { id: string; how: string }[];
  deploy?: Deploy | null;
}

export interface TreeAgent {
  id: string;
  shortId: string;
  title: string | null;
  provider: string;
  status: string;
  requiresAttention?: boolean | null;
}

export interface TreeWorkspace {
  workspaceId: string;
  name: string;
  branch?: string | null;
}

export interface TreeProject {
  id: string;
  group: string;
  level: Effective;
  paths: string[];
  components: CatalogComponent[];
  agents?: TreeAgent[];
  workspaces?: TreeWorkspace[];
}

export interface HostStatus {
  reachable: boolean;
  error?: string | null;
  uptime: string;
  load: string;
  tctl?: number | null;
}

export interface CatalogTree {
  generated_at: string | null;
  hosts: Record<string, HostStatus> | null;
  drift: { unknown: number; missing: number };
  counts: Record<string, number>;
  groups: { name: string; level: Effective; projects: TreeProject[] }[];
}

export interface Impacted {
  id: string;
  effect: "broken" | "degraded";
  via: string[];
  criticality: string;
  serves: string[];
}

export interface Impact {
  down: string[];
  broken: Impacted[];
  degraded: Impacted[];
  public_hostnames_broken: string[];
}

export const catalogApi = {
  tree: () => request<CatalogTree>("GET", B, { timeoutMs: 30_000 }),
  component: (id: string) => request<CatalogComponent>("GET", `${B}/components/${enc(id)}`),
  impact: (ids: string[]) => request<Impact>("GET", `${B}/impact/${enc(ids.join(","))}`),
  /** Rescans the component's host first (up to ~2 minutes). */
  checkNow: (id: string) =>
    request<{ id: string; generated_at: string; status: ComponentStatus }>("GET", `${B}/health?id=${enc(id)}&live=true`, { timeoutMs: 170_000 }),
};

/** Worst first, as the tree sorts. */
export function rank(e: Effective | undefined): number {
  switch (e) {
    case "down":
    case "broken":
      return 4;
    case "warn":
    case "degraded":
      return 3;
    case "unknown":
      return 2;
    case "ok":
      return 1;
    default:
      return 0;
  }
}
