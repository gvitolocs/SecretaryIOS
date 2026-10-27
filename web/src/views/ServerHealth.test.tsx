import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { HealthCheck, HealthEvent, SystemHealth } from "../api/systemHealth";
import { healthyByGroup, needsAttention } from "../api/systemHealth";
import { setLocale } from "../i18n";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import ServerHealthView, { duration, REFRESH_MS } from "./ServerHealth";

const ago = (min: number) => new Date(Date.now() - min * 60_000).toISOString();

function check(over: Partial<HealthCheck> & { id: string }): HealthCheck {
  return { kind: "unit", name: over.id.split(":")[1]!, group: "Lucina", state: "ok", detail: "active", since: ago(600), restarts_1h: 0, ...over };
}

function health(over: Partial<SystemHealth> = {}, stale = false): SystemHealth {
  return {
    hosts: [
      {
        host: "nezopt",
        received_at: ago(stale ? 40 : 2),
        stale,
        snapshot: {
          host: "nezopt",
          at: ago(stale ? 40 : 2),
          overall: "crit",
          metrics: {
            disks: [
              { mount: "/", used_pct: 59.1, free_gb: 736 },
              { mount: "/data", used_pct: 96, free_gb: 12 },
            ],
            cpu_temp_c: 71.5,
            load: [1.2, 1.4, 1.6],
            mem_used_pct: 62,
            swap_used_pct: 10,
          },
          checks: [
            check({ id: "unit:lucina-rs", state: "crit", detail: "restarted 12 times in 1 h", since: ago(90), restarts_1h: 12 }),
            check({ id: "timer:backup", kind: "timer", state: "warn", detail: "last run 26 h ago", group: "Homelab", since: ago(30) }),
            check({ id: "unit:kokoro", group: "AI" }),
            check({ id: "container:pokoin-api", kind: "container", group: "Pokoin" }),
            check({ id: "unit:secretary", group: "Lucina" }),
          ],
        },
      },
    ],
    events: [
      { at: ago(90), host: "nezopt", check_id: "unit:lucina-rs", name: "lucina-rs", from: "ok", to: "crit", detail: "restarted 12 times in 1 h" },
      { at: ago(200), host: "nezopt", check_id: "unit:kokoro", name: "kokoro", from: "crit", to: "ok", detail: "active" },
    ],
    ...over,
  };
}

const show = () => renderAt("/server-health", () => <Route path="/server-health" component={ServerHealthView} />);

describe("ServerHealth", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("shows hosts, metrics, the checks needing attention and the history", async () => {
    const calls = stubFetch(() => ({ body: health() }));
    show();
    const host = await screen.findByText("nezopt");
    expect(calls[0]!.url).toContain("/v1/system/health");
    const head = host.closest("li")!;
    expect(head).toHaveAttribute("data-state", "crit");
    expect(within(head).getByText("Critical")).toBeInTheDocument();

    expect(screen.getByText("Free on /")).toBeInTheDocument();
    expect(screen.getByText("Free on /data").closest(".sh-tile")).toHaveAttribute("data-level", "crit");
    expect(screen.getByText("71.5 °C")).toBeInTheDocument();
    expect(screen.getByText("62%")).toBeInTheDocument();

    const attention = screen.getByRole("list", { name: "Needs attention" });
    const items = within(attention).getAllByRole("listitem");
    expect(items.map((li) => li.getAttribute("data-state"))).toEqual(["crit", "warn"]);
    expect(within(items[0]!).getByText("lucina-rs")).toBeInTheDocument();
    expect(within(items[0]!).getByText("· 12 restarts in 1 h")).toBeInTheDocument();
    expect(within(items[0]!).getByText(/^for 1 hr/)).toBeInTheDocument();
    expect(within(items[1]!).getByText("Homelab")).toBeInTheDocument();

    // Healthy checks are collapsed by default and grouped.
    const healthy = screen.getByText("All good · 3 checks").closest("details")!;
    expect(healthy.open).toBe(false);
    expect(within(healthy).getAllByRole("heading", { level: 3 }).map((h) => h.textContent)).toEqual(["Lucina", "Pokoin", "AI"]);

    const history = within(screen.getByRole("list", { name: "History" })).getAllByRole("listitem");
    expect(history.map((li) => li.getAttribute("data-to"))).toEqual(["crit", "ok"]);
  });

  it("shows a stale host as unreachable", async () => {
    stubFetch(() => ({ body: health({}, true) }));
    show();
    const head = (await screen.findByText("nezopt")).closest("li")!;
    expect(head).toHaveAttribute("data-state", "stale");
    expect(within(head).getByText(/^Unreachable since /)).toBeInTheDocument();
  });

  it("handles an empty history and no hosts", async () => {
    stubFetch(() => ({ body: { hosts: [], events: [] } }));
    show();
    expect(await screen.findByText("No server has reported yet.")).toBeInTheDocument();
    expect(screen.getByText("No changes in the last 30 days.")).toBeInTheDocument();
  });

  it("pages the history 50 at a time", async () => {
    const events: HealthEvent[] = Array.from({ length: 120 }, (_, i) => ({
      at: ago(i + 1),
      host: "nezopt",
      check_id: `unit:s${i}`,
      name: `s${i}`,
      from: "ok",
      to: "warn",
      detail: "",
    }));
    stubFetch(() => ({ body: health({ events }) }));
    show();
    const list = await screen.findByRole("list", { name: "History" });
    expect(within(list).getAllByRole("listitem")).toHaveLength(50);
    fireEvent.click(screen.getByRole("button", { name: "Show more" }));
    expect(within(list).getAllByRole("listitem")).toHaveLength(100);
    fireEvent.click(screen.getByRole("button", { name: "Show more" }));
    expect(within(list).getAllByRole("listitem")).toHaveLength(120);
    expect(screen.queryByRole("button", { name: "Show more" })).toBeNull();
  });

  it("shows an error with retry, and refreshes on demand and every minute", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let fail = true;
    const calls = stubFetch(() => (fail ? { status: 500, body: { error: { code: "internal", message: "boom" } } } : { body: health() }));
    show();
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't load server health.");
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByText("nezopt");
    expect(screen.queryByRole("alert")).toBeNull();
    const n = calls.length;
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(calls.length).toBe(n + 1));
    await vi.advanceTimersByTimeAsync(REFRESH_MS);
    await waitFor(() => expect(calls.length).toBe(n + 2));
  });

  it("is translated", async () => {
    setLocale("it");
    stubFetch(() => ({ body: health() }));
    show();
    expect(await screen.findByText("Da controllare")).toBeInTheDocument();
    expect(screen.getByText("Cronologia")).toBeInTheDocument();
  });
});

describe("server health helpers", () => {
  it("orders problems crit first and groups healthy checks", () => {
    const checks = [
      check({ id: "unit:b", state: "warn", since: ago(100) }),
      check({ id: "unit:a", state: "crit", since: ago(5) }),
      check({ id: "unit:z", group: "Other" }),
      check({ id: "unit:c", group: "CardRails" }),
    ];
    expect(needsAttention(checks).map((c) => c.id)).toEqual(["unit:a", "unit:b"]);
    expect(healthyByGroup(checks).map((g) => g.group)).toEqual(["CardRails", "Other"]);
  });

  it("formats durations", () => {
    const now = Date.parse("2026-10-27T12:00:00Z");
    expect(duration("2026-10-27T11:48:00Z", now, "en")).toBe("12 min");
    expect(duration("2026-10-27T09:00:00Z", now, "en")).toBe("3 hr");
    expect(duration("2026-10-24T12:00:00Z", now, "en")).toBe("3 days");
  });
});
