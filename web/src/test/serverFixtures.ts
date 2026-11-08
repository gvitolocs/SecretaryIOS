import type { AttentionItem, Host, SystemOverview, TimelineItem } from "../api/systemOverview";

// A /v1/system/overview fixture for the Server page tests.

export const ago = (min: number) => new Date(Date.now() - min * 60_000).toISOString();

const series = (f: (i: number) => number | null) => Array.from({ length: 96 }, (_, i) => f(i));

export function host(over: Partial<Host> & { id: string }): Host {
  return {
    name: over.id,
    role: null,
    state: "ok",
    reachable: true,
    limited: false,
    last_report: ago(2),
    uptime: "23d 4h",
    os: "Ubuntu 26.04.1 LTS",
    kernel: "Linux 7.0.0-42-generic",
    cpus: 16,
    load: [9.58, 5.03, 2.48],
    mem_used_pct: 33.3,
    tctl: 95.1,
    disks: [{ mount: "/", used_pct: 77.6, free_gb: 422, size_gb: 1885 }],
    history: {
      step_s: 900,
      end: new Date(Math.ceil(Date.now() / 900_000) * 900_000).toISOString(),
      tctl: series((i) => (i === 40 ? null : 61 + (i % 36))),
      load1: series((i) => 1 + (i % 9)),
      mem: series(() => 33),
    },
    findings: [],
    component_id: over.id,
    ...over,
  };
}

export function item(over: Partial<AttentionItem> & { key: string; title: string }): AttentionItem {
  return {
    sources: ["catalog"],
    severity: "warn",
    component_id: null,
    host: "nezopt",
    group: "Lucina",
    project: "secretary",
    kind: "service",
    criticality: "medium",
    summary: "Something is off",
    details: [],
    since: ago(130),
    failures_24h: null,
    impact: null,
    caused: [],
    logs: null,
    restart: null,
    runbook: null,
    ...over,
  };
}

export function event(over: Partial<TimelineItem> & { title: string; kind: TimelineItem["kind"]; at: string }): TimelineItem {
  return { project: null, host: null, severity: null, from: null, to: null, detail: null, ops_id: null, ...over };
}

export function overview(over: Partial<SystemOverview> = {}): SystemOverview {
  return {
    generated_at: ago(1),
    overall: "crit",
    attention_count: 2,
    checks_ok: 122,
    checks_total: 150,
    sources: {
      catalog: { ok: true, at: ago(5), error: null },
      sentinel: { ok: true, at: ago(2), error: null },
      ops: { ok: true, at: ago(2), error: null },
    },
    hosts: [
      host({ id: "nezopt", role: "home server", state: "warn", findings: ["Tctl 95 °C"], disks: [{ mount: "/", used_pct: 77.6, free_gb: 422, size_gb: 1885 }, { mount: "/data", used_pct: 93, free_gb: 70, size_gb: 1000 }] }),
      host({ id: "nespc", state: "ok", limited: true, history: null, load: null, tctl: null, mem_used_pct: null, disks: [] }),
      host({ id: "pi-home", state: "crit", reachable: false, last_report: ago(300), history: null }),
    ],
    attention: [
      item({
        key: "catalog:gmaps-server",
        severity: "crit",
        title: "gmaps-server",
        component_id: "gmaps-server",
        project: "secretary",
        summary: "Not running; port 18210 not listening",
        details: ["user unit gmaps-server.service: inactive/dead", "port 18210: not listening"],
        failures_24h: 142,
        impact: { broken: [{ id: "lucina-maps", name: "Lucina Maps" }], degraded: [], public_hostnames_broken: ["maps.example.com"] },
        caused: ["lucina-maps"],
        logs: "journalctl --user -u gmaps-server -n 100 --no-pager",
        restart: "systemctl --user restart gmaps-server",
        runbook: "## Fix\nRestart it. <script>alert(1)</script>",
      }),
      item({ key: "sentinel:nezopt:timer:backup", title: "backup", project: "homelab", summary: "Last run 26 h ago" }),
    ],
    services: [
      {
        name: "Lucina",
        state: "crit",
        projects: [
          {
            id: "secretary",
            state: "crit",
            components: [
              { id: "gmaps-server", name: "gmaps-server", kind: "service", host: "nezopt", state: "crit", criticality: "medium", description: "Maps backend" },
              { id: "lucina-maps", name: "Lucina Maps", kind: "service", host: "nezopt", state: "crit", criticality: "low", description: null },
              { id: "secretary-server", name: "Secretary server", kind: "service", host: "nezopt", state: "ok", criticality: "critical", description: "The API" },
            ],
          },
        ],
      },
      {
        name: "Pokoin",
        state: "ok",
        projects: [
          { id: "pokoin", state: "ok", components: [{ id: "pokoin-api", name: "Pokoin API", kind: "container", host: "p1", state: "ok", criticality: "high", description: "The Pokoin API" }] },
        ],
      },
    ],
    timeline: [
      event({ at: ago(10), kind: "change", title: "gmaps-server went down", host: "nezopt", from: "ok", to: "crit" }),
      event({ at: ago(30), kind: "deploy", title: "secretary-server deployed", project: "secretary" }),
      event({ at: ago(60), kind: "incident", title: "nezopt overheating", project: "nezopt", severity: "high" }),
      event({ at: ago(60 * 24 * 3), kind: "decision", title: "Move backups to p1", project: "homelab" }),
    ],
    ...over,
  };
}
