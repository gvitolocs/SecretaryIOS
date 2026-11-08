// GET /v1/system/overview for the mock. MOCK_OVERVIEW_FILE serves a captured response (e.g. one built from real
// catalog/sentinel/ops data for screenshots); without it, a small calm fixture. MOCK_OVERVIEW=empty: no hosts.

import { readFileSync } from "node:fs";

const ago = (min) => new Date(Date.now() - min * 60_000).toISOString();
const flat = (v) => Array.from({ length: 96 }, (_, i) => Math.round((v + Math.sin(i / 7) * v * 0.15) * 10) / 10);

function fixture() {
  if (process.env.MOCK_OVERVIEW_FILE) return JSON.parse(readFileSync(process.env.MOCK_OVERVIEW_FILE, "utf8"));
  const empty = process.env.MOCK_OVERVIEW === "empty";
  const end = new Date(Math.ceil(Date.now() / 900_000) * 900_000).toISOString();
  return {
    generated_at: ago(0),
    overall: "ok",
    attention_count: 0,
    flapping_count: 0,
    checks_ok: empty ? 0 : 12,
    checks_total: empty ? 0 : 12,
    sources: { catalog: { ok: true, at: ago(3), error: null }, sentinel: { ok: true, at: ago(2), error: null }, ops: { ok: true, at: ago(0), error: null } },
    hosts: empty
      ? []
      : [
          {
            id: "nezopt", name: "nezopt", role: "home server", state: "ok", reachable: true, limited: false, last_report: ago(2),
            uptime: "3d 4h", os: "Ubuntu 26.04.1 LTS", kernel: "Linux 7.0.0-42-generic", cpus: 12, load: [1.2, 1.4, 1.6],
            mem_used_pct: 41.5, tctl: 64, disks: [{ mount: "/", used_pct: 59.1, free_gb: 736, size_gb: 1800 }],
            history: { step_s: 900, end, tctl: flat(62), load1: flat(1.4), mem: flat(40) }, findings: [], component_id: "nezopt",
          },
        ],
    attention: [],
    services: [],
    timeline: [],
  };
}

export async function handleSystemOverview(req, res, { path, method, send }) {
  if (path !== "/v1/system/overview" || method !== "GET") return false;
  send(res, 200, fixture());
  return true;
}
