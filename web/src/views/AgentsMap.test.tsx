import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { CatalogComponent, CatalogTree } from "../api/catalog";
import { resetAgentsState } from "../state/agents";
import { installEventSource } from "../test/eventsource";
import { serveAgents } from "../test/agentsFixtures";
import { renderAt } from "../test/router";
import AgentsView from "./Agents";

// The service map at /agents/map: the Homelab catalog through /v1/agents/catalog (agentd → the catalog CLI).

function renderAgents(path: string) {
  return renderAt(path, () => <Route path="/agents/*rest" component={AgentsView} />);
}

beforeEach(() => {
  installEventSource();
});
afterEach(() => {
  resetAgentsState();
});

const status = (effective: string, findings: { level: string; what: string; detail: string }[] = [], impacted_by: string[] = []) => ({
  level: effective === "broken" || effective === "degraded" ? "ok" : effective,
  observed: effective,
  since: "2026-10-28T16:30:00Z",
  effective,
  impacted_by,
  findings,
  failures_24h: 0,
});

const writer: CatalogComponent = {
  id: "pokoin-db-writer",
  name: "Pokoin Postgres writer (nezopt)",
  kind: "db",
  group: "Pokoin",
  project: "pokoin",
  host: "nezopt",
  criticality: "critical",
  containers: ["pokoin-marketplace-postgres-15t"],
  depends_on: ["docker-nezopt"],
  restart: "docker restart pokoin-marketplace-postgres-15t",
  runbook: "Reads keep working from the Pi replica.",
  status: status("down", [{ level: "down", what: "container pokoin-marketplace-postgres-15t", detail: "Exited (0) 1 minute ago" }]) as never,
  dependents: [{ id: "k3s-pokoin-api", how: "hard" }],
};
const k3sApi: CatalogComponent = {
  id: "k3s-pokoin-api",
  kind: "k8s",
  group: "Pokoin",
  project: "pokoin",
  host: "nezopt",
  criticality: "high",
  status: status("broken", [], ["pokoin-db-writer"]) as never,
};
const lucina: CatalogComponent = {
  id: "lucina-rs",
  name: "Lucina (Rust)",
  kind: "service",
  group: "Lucina",
  project: "lucina",
  host: "nezopt",
  criticality: "critical",
  status: status("ok", [{ level: "ok", what: "GET http://127.0.0.1:8790/health", detail: "HTTP 200" }]) as never,
};

const TREE: CatalogTree = {
  generated_at: "2026-10-28T16:35:00Z",
  hosts: { nezopt: { reachable: true, uptime: "4h", load: "1 1 1", tctl: 71 }, "pi-home": { reachable: false, uptime: "", load: "", error: "timeout" } },
  drift: { unknown: 0, missing: 0 },
  counts: { ok: 1, down: 1, broken: 1 },
  groups: [
    { name: "Lucina", level: "ok", projects: [{ id: "lucina", group: "Lucina", level: "ok", paths: [], components: [lucina], agents: [{ id: "a1", shortId: "a1", title: "Fix the build", provider: "claude", status: "running" }], workspaces: [] }] },
    { name: "Pokoin", level: "down", projects: [{ id: "pokoin", group: "Pokoin", level: "down", paths: [], components: [writer, k3sApi], agents: [], workspaces: [] }] },
  ],
};

function serveCatalog() {
  return serveAgents({
    absent: ["/search", "/schedules"],
    extra: (call) => {
      const path = new URL(call.url, "http://x").pathname;
      if (path === "/v1/agents/catalog") return { body: TREE };
      if (path === "/v1/agents/catalog/components/pokoin-db-writer") return { body: writer };
      if (path === "/v1/agents/catalog/impact/pokoin-db-writer")
        return {
          body: {
            down: ["pokoin-db-writer"],
            broken: [{ id: "k3s-pokoin-api", effect: "broken", via: ["pokoin-db-writer"], criticality: "high", serves: [] }],
            degraded: [{ id: "site-api", effect: "degraded", via: ["cloudflared-pi", "pokoin-db-writer"], criticality: "critical", serves: ["api.pokoin.com"] }],
            public_hostnames_broken: [],
          },
        };
      if (path === "/v1/agents/catalog/health")
        return { body: { id: "pokoin-db-writer", generated_at: "2026-10-28T16:36:00Z", status: status("ok", [{ level: "ok", what: "container pokoin-marketplace-postgres-15t", detail: "Up 5 seconds (healthy)" }]) } };
      return undefined;
    },
  });
}

describe("service map", () => {
  it("puts what is broken first, shows hosts and the agents of each project", async () => {
    serveCatalog();
    renderAgents("/agents/map");
    const problems = await screen.findByRole("list", { name: "Components that are not healthy" });
    const rows = within(problems).getAllByRole("link");
    expect(rows.map((r) => r.getAttribute("href"))).toEqual(["/agents/map/pokoin-db-writer", "/agents/map/k3s-pokoin-api"]);
    expect(within(rows[0]!).getByText(/container pokoin-marketplace-postgres-15t: Exited/)).toBeInTheDocument();
    expect(within(rows[1]!).getByText("because of pokoin-db-writer")).toBeInTheDocument();
    const hosts = screen.getByRole("list", { name: "Hosts" });
    expect(within(hosts).getByText("unreachable")).toBeInTheDocument();
    // The Lucina project carries its running agent, linked to the agent page.
    expect(screen.getByRole("link", { name: "Fix the build" })).toHaveAttribute("href", "/agents/a1");
    expect(screen.getByText("1 agents")).toBeInTheDocument();
  });

  it("filters to problems only", async () => {
    serveCatalog();
    renderAgents("/agents/map");
    await screen.findByRole("list", { name: "Components that are not healthy" });
    expect(screen.getByRole("list", { name: "lucina" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("checkbox", { name: "Only problems" }));
    await waitFor(() => expect(screen.queryByRole("list", { name: "lucina" })).not.toBeInTheDocument());
    expect(within(screen.getByRole("list", { name: "pokoin" })).getAllByRole("link")).toHaveLength(2);
  });

  it("opens a component: why it is down, what to run, what breaks, and a live recheck", async () => {
    const calls = serveCatalog();
    renderAgents("/agents/map/pokoin-db-writer");
    expect(await screen.findByRole("heading", { name: /Pokoin Postgres writer/ })).toBeInTheDocument();
    const state = () => document.querySelector(".ag-cat-state");
    expect(state()).toHaveTextContent("Down");
    expect(screen.getByText("docker restart pokoin-marketplace-postgres-15t")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "k3s-pokoin-api" })).toHaveAttribute("href", "/agents/map/k3s-pokoin-api");

    fireEvent.click(screen.getByRole("button", { name: "Show what breaks" }));
    const impact = await screen.findByRole("list", { name: "If it goes down" });
    expect(within(impact).getAllByRole("link").map((l) => l.getAttribute("href"))).toEqual(["/agents/map/k3s-pokoin-api", "/agents/map/site-api"]);
    expect(within(impact).getByText(/api\.pokoin\.com/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Check now" }));
    await waitFor(() => expect(state()).toHaveTextContent("Healthy"));
    expect(calls.some((c) => c.url === "/v1/agents/catalog/health?id=pokoin-db-writer&live=true")).toBe(true);
  });
});
