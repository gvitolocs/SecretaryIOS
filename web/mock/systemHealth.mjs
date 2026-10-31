// GET /v1/system/health for the mock: one healthy-ish host with a crashing unit and a late
// timer, and a stale second host. MOCK_HEALTH=empty answers no hosts.

const ago = (min) => new Date(Date.now() - min * 60_000).toISOString();

const check = (id, group, state = "ok", detail = "active", since = ago(3000), restarts = 0) => {
  const [kind, name] = id.split(":");
  return { id, kind, name: name.replace(/\.service$/, ""), group, state, detail, since, restarts_1h: restarts };
};

function fixture() {
  if (process.env.MOCK_HEALTH === "empty") return { hosts: [], events: [] };
  const nezopt = {
    host: "nezopt",
    at: ago(2),
    overall: "crit",
    metrics: {
      disks: [
        { mount: "/", used_pct: 59.1, free_gb: 736 },
        { mount: "/mnt/media", used_pct: 91.4, free_gb: 160 },
      ],
      cpu_temp_c: 71.5,
      load: [1.2, 1.4, 1.6],
      mem_used_pct: 62,
      swap_used_pct: 10,
    },
    checks: [
      check("unit:lucina-rs.service", "Lucina", "crit", "restarted 12 times in 1 h", ago(47), 12),
      check("timer:restic-backup.timer", "Homelab", "warn", "last run 26 h ago", ago(140)),
      check("unit:secretary-mcp.service", "Lucina"),
      check("unit:hermes.service", "Lucina"),
      check("container:pokoin-api", "Pokoin"),
      check("k3s:cardrails/api", "CardRails"),
      check("unit:kokoro.service", "AI"),
      check("unit:ollama.service", "AI"),
      check("forward:p1-relay", "Homelab", "ok", "127.0.0.1:18788 open"),
      check("disk:/", "Other", "ok", "59% used"),
    ],
  };
  const p1 = {
    host: "p1",
    at: ago(42),
    overall: "ok",
    metrics: { disks: [{ mount: "/", used_pct: 40, free_gb: 28 }], cpu_temp_c: null, load: [0.1, 0.1, 0.2], mem_used_pct: 35, swap_used_pct: 0 },
    checks: [check("unit:secretary-server.service", "Lucina")],
  };
  return {
    hosts: [
      { host: "nezopt", received_at: ago(2), snapshot: nezopt, stale: false },
      { host: "p1", received_at: ago(42), snapshot: p1, stale: true },
    ],
    events: [
      { at: ago(47), host: "nezopt", check_id: "unit:lucina-rs.service", name: "lucina-rs", from: "ok", to: "crit", detail: "restarted 12 times in 1 h" },
      { at: ago(140), host: "nezopt", check_id: "timer:restic-backup.timer", name: "restic-backup", from: "ok", to: "warn", detail: "last run 26 h ago" },
      { at: ago(600), host: "nezopt", check_id: "unit:kokoro.service", name: "kokoro", from: "crit", to: "ok", detail: "active" },
      { at: ago(640), host: "nezopt", check_id: "unit:kokoro.service", name: "kokoro", from: "ok", to: "crit", detail: "failed (exit 1)" },
    ],
  };
}

export async function handleSystemHealth(req, res, { path, method, send }) {
  if (path !== "/v1/system/health" || method !== "GET") return false;
  send(res, 200, fixture());
  return true;
}
