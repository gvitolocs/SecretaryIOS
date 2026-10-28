import { request } from "./client";

// The Server page (/server-health): GET /v1/system/overview, one document with the hosts (latest
// metrics and 24 h history), what needs attention (catalog problems and sentinel checks merged),
// the catalog tree and the timeline (incidents, deploys, decisions, state changes). `?fresh=1`
// skips the server's 30 s cache. A failing source shows up in `sources`; the rest still renders.

export type Overall = "ok" | "warn" | "crit";
export type HostState = "ok" | "warn" | "crit" | "unknown";
export type ServiceState = "ok" | "warn" | "crit" | "idle" | "unknown";
export type Criticality = "low" | "medium" | "high" | "critical";
export type TimelineKind = "incident" | "deploy" | "decision" | "change";

export interface SourceStatus {
  ok: boolean;
  at: string | null;
  error: string | null;
}

export type SourceName = "catalog" | "sentinel" | "ops";

export interface Disk {
  mount: string;
  used_pct: number;
  free_gb: number;
  size_gb: number;
}

export interface HostHistory {
  step_s: number;
  /** End of the last bucket. */
  end: string;
  tctl: (number | null)[];
  load1: (number | null)[];
  mem: (number | null)[];
}

export interface Host {
  id: string;
  name: string;
  role: string | null;
  state: HostState;
  reachable: boolean;
  limited: boolean;
  last_report: string | null;
  uptime: string | null;
  os: string | null;
  kernel: string | null;
  cpus: number | null;
  load: [number, number, number] | null;
  mem_used_pct: number | null;
  tctl: number | null;
  disks: Disk[];
  history: HostHistory | null;
  findings: string[];
  component_id: string | null;
}

export interface Ref {
  id: string;
  name: string;
}

export interface Impact {
  broken: Ref[];
  degraded: Ref[];
  public_hostnames_broken: string[];
}

export interface AttentionItem {
  key: string;
  sources: string[];
  severity: "crit" | "warn";
  title: string;
  component_id: string | null;
  host: string | null;
  group: string | null;
  project: string | null;
  kind: string | null;
  criticality: Criticality | null;
  summary: string;
  details: string[];
  since: string;
  failures_24h: number | null;
  flapping?: boolean;
  impact: Impact | null;
  caused: string[];
  logs: string | null;
  restart: string | null;
  runbook: string | null;
}

export interface ServiceComponent {
  id: string;
  name: string;
  kind: string;
  host: string | null;
  state: ServiceState;
  criticality: Criticality | null;
  description: string | null;
}

export interface ServiceProject {
  id: string;
  state: ServiceState;
  components: ServiceComponent[];
}

export interface ServiceGroup {
  name: string;
  state: ServiceState;
  projects: ServiceProject[];
}

export interface TimelineItem {
  at: string;
  kind: TimelineKind;
  title: string;
  project: string | null;
  host: string | null;
  severity: Criticality | null;
  from: Overall | null;
  to: Overall | null;
  detail: string | null;
  ops_id: string | null;
}

export interface SystemOverview {
  generated_at: string;
  overall: Overall;
  attention_count: number;
  flapping_count?: number;
  checks_ok: number;
  checks_total: number;
  sources: Record<SourceName, SourceStatus>;
  hosts: Host[];
  attention: AttentionItem[];
  services: ServiceGroup[];
  /** Newest first. */
  timeline: TimelineItem[];
}

export const systemOverviewApi = {
  get: (fresh = false) => request<SystemOverview>("GET", fresh ? "/v1/system/overview?fresh=1" : "/v1/system/overview"),
};

// ---- Helpers ----------------------------------------------------------------------------------

/** "12 min", "3 hr", "2 days": how long since `iso`, without "ago" (for "for 2 h"). */
export function duration(iso: string, now = Date.now(), loc = "en"): string {
  const min = Math.max(0, Math.floor((now - Date.parse(iso)) / 60_000));
  const [value, unit] = min < 60 ? [min, "minute"] : min < 48 * 60 ? [Math.floor(min / 60), "hour"] : [Math.floor(min / 1440), "day"];
  try {
    return new Intl.NumberFormat(loc, { style: "unit", unit, unitDisplay: "short" }).format(value);
  } catch {
    return `${value} ${unit}`;
  }
}

/** A disk bar's level: ≥ 90 % used is crit, ≥ 80 % warn, else ok. */
export function diskLevel(usedPct: number): "ok" | "warn" | "crit" {
  return usedPct >= 90 ? "crit" : usedPct >= 80 ? "warn" : "ok";
}

/** The server's count excludes recovered, flapping items and extra host counts. */
export function attentionTotal(o: SystemOverview): number {
  return o.attention_count;
}

export function flappingTotal(o: SystemOverview): number {
  return o.flapping_count ?? o.attention.filter((a) => a.flapping).length;
}

/** The local calendar day of `iso` as "YYYY-MM-DD". */
export function dayKey(iso: string | number): string {
  const d = new Date(iso);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** "today", "yesterday" or the day itself, for a day heading. */
export function dayRelation(key: string, now = Date.now()): "today" | "yesterday" | "other" {
  if (key === dayKey(now)) return "today";
  const y = new Date(now);
  y.setDate(y.getDate() - 1);
  return key === dayKey(y.getTime()) ? "yesterday" : "other";
}

/** Timeline items of the given kinds grouped by local day, newest day first, order kept inside a day. */
export function groupByDay(items: TimelineItem[], kinds?: TimelineKind[]): { day: string; items: TimelineItem[] }[] {
  const out: { day: string; items: TimelineItem[] }[] = [];
  const sorted = items.filter((i) => !kinds || kinds.includes(i.kind)).sort((a, b) => Date.parse(b.at) - Date.parse(a.at));
  for (const item of sorted) {
    const day = dayKey(item.at);
    const last = out[out.length - 1];
    if (last && last.day === day) last.items.push(item);
    else out.push({ day, items: [item] });
  }
  return out;
}

const UNHEALTHY: ServiceState[] = ["warn", "crit"];
export const isUnhealthy = (s: ServiceState) => UNHEALTHY.includes(s);

/**
 * The catalog tree trimmed to components matching `query` (id, name, host or project, case-insensitive).
 * A project whose id matches keeps all its components. An empty query returns the tree.
 */
export function filterServices(groups: ServiceGroup[], query: string): ServiceGroup[] {
  const q = query.trim().toLowerCase();
  if (!q) return groups;
  const has = (s: string | null | undefined) => !!s && s.toLowerCase().includes(q);
  const out: ServiceGroup[] = [];
  for (const g of groups) {
    const projects: ServiceProject[] = [];
    for (const p of g.projects) {
      const components = has(p.id) ? p.components : p.components.filter((c) => has(c.id) || has(c.name) || has(c.host));
      if (components.length) projects.push({ ...p, components });
    }
    if (projects.length) out.push({ ...g, projects });
  }
  return out;
}

/** Every component in the tree with its group and project. */
export function allComponents(groups: ServiceGroup[]): (ServiceComponent & { group: string; project: string })[] {
  return groups.flatMap((g) => g.projects.flatMap((p) => p.components.map((c) => ({ ...c, group: g.name, project: p.id }))));
}

/** The healthy (ok / idle) components grouped by group name, groups in tree order, names sorted. */
export function healthyByGroup(groups: ServiceGroup[]): { group: string; components: ServiceComponent[] }[] {
  return groups
    .map((g) => ({
      group: g.name,
      components: g.projects
        .flatMap((p) => p.components)
        .filter((c) => c.state === "ok" || c.state === "idle")
        .sort((a, b) => a.name.localeCompare(b.name)),
    }))
    .filter((g) => g.components.length > 0);
}

/** The attention item for a component: `catalog:<id>` or any item naming it. */
export function attentionFor(items: AttentionItem[], componentId: string): AttentionItem | undefined {
  return items.find((i) => i.key === `catalog:${componentId}`) ?? items.find((i) => i.component_id === componentId);
}

// ---- Charts -----------------------------------------------------------------------------------

export interface SeriesStats {
  now: number | null;
  min: number | null;
  max: number | null;
}

/** The last non-null value and the min / max of a series. */
export function seriesStats(values: (number | null)[]): SeriesStats {
  const real = values.filter((v): v is number => v != null && Number.isFinite(v));
  if (!real.length) return { now: null, min: null, max: null };
  return { now: real[real.length - 1]!, min: Math.min(...real), max: Math.max(...real) };
}

/**
 * The y range of a chart: `fixed` when given (temperature 30–100 °C, memory 0–100 %), else the
 * series min / max padded by 10 % of the range (or 0.5 when flat), never below `floor`.
 */
export function chartScale(values: (number | null)[], opts: { fixed?: [number, number]; floor?: number } = {}): [number, number] {
  if (opts.fixed) return opts.fixed;
  const { min, max } = seriesStats(values);
  if (min == null || max == null) return [0, 1];
  const pad = max > min ? (max - min) * 0.1 : 0.5;
  const lo = opts.floor != null ? Math.max(opts.floor, min - pad) : min - pad;
  return [lo, max + pad];
}

export interface ChartPaths {
  /** The line, one `M … L …` run per stretch without gaps. */
  line: string;
  /** The filled area under each run, closed at the bottom. */
  area: string;
  /** The last value's position, 0–1 from the left and from the top. */
  last: { x: number; y: number } | null;
}

const r2 = (n: number) => Math.round(n * 100) / 100;

/**
 * SVG paths for a series in a `w` × `h` box (y from `lo` at the bottom to `hi` at the top, values
 * clamped). Nulls break the line; fewer than two samples produce no paths.
 */
export function chartPaths(values: (number | null)[], w: number, h: number, [lo, hi]: [number, number]): ChartPaths {
  const n = values.length;
  if (values.filter((v) => v != null && Number.isFinite(v)).length < 2) return { line: "", area: "", last: null };
  const xAt = (i: number) => (n <= 1 ? w : (i / (n - 1)) * w);
  const yAt = (v: number) => {
    const f = hi > lo ? (Math.min(hi, Math.max(lo, v)) - lo) / (hi - lo) : 0.5;
    return h - f * h;
  };
  const runs: [number, number][][] = [];
  let run: [number, number][] = [];
  values.forEach((v, i) => {
    if (v == null || !Number.isFinite(v)) {
      if (run.length) runs.push(run);
      run = [];
    } else run.push([xAt(i), yAt(v)]);
  });
  if (run.length) runs.push(run);
  const line = runs
    .filter((pts) => pts.length > 1)
    .map((pts) => {
      return pts.map(([x, y], i) => `${i ? "L" : "M"}${r2(x)} ${r2(y)}`).join(" ");
    })
    .join(" ");
  const area = runs
    .filter((pts) => pts.length > 1)
    .map((pts) => `M${r2(pts[0]![0])} ${h} ` + pts.map(([x, y]) => `L${r2(x)} ${r2(y)}`).join(" ") + ` L${r2(pts[pts.length - 1]![0])} ${h} Z`)
    .join(" ");
  const lastRun = runs[runs.length - 1];
  const lp = lastRun?.[lastRun.length - 1];
  return { line, area, last: lp ? { x: w ? lp[0] / w : 0, y: h ? lp[1] / h : 0 } : null };
}

/**
 * Hour ticks for a history window: every `everyH` hours on the local clock (00:00, 06:00, …)
 * inside the window, each with its position 0–1 from the left.
 */
export function hourTicks(history: Pick<HostHistory, "end" | "step_s"> & { count: number }, everyH = 6): { at: number; x: number }[] {
  const end = Date.parse(history.end);
  const span = history.count * history.step_s * 1000;
  const start = end - span;
  const first = new Date(start);
  first.setMinutes(0, 0, 0);
  if (first.getTime() < start) first.setHours(first.getHours() + 1);
  const out: { at: number; x: number }[] = [];
  for (let t = first.getTime(); t <= end; t += 3_600_000) {
    if (new Date(t).getHours() % everyH === 0) out.push({ at: t, x: span ? (t - start) / span : 0 });
  }
  return out;
}
