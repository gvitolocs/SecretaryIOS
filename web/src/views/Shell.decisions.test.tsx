import { Route, type RouteSectionProps } from "@solidjs/router";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Decision } from "../api/decisions";
import { resetDecisions } from "../state/decisions";
import { MockEventSource } from "../test/eventsource";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import Shell from "./Shell";

const decision = (id: string, question: string): Decision => ({
  decision_id: id, status: "pending", decision: null, chosen_option: null, chosen_index: null, reason: null,
  decided_by: null, confidence: null, asked_owner: true, owner_only: true, question, options: ["Go", "No"],
  recommendation: null, impact: "medium", reversible: true, urgency: "normal", topic: null, asking_client: "claude-code",
  asking_session: null, created_at: "2026-10-28T08:00:00Z", decided_at: null, expires_at: null, basis: null, evidence: [],
});

function serve(pending: () => Decision[]) {
  return stubFetch((c) => {
    if (c.method === "GET" && c.url.startsWith("/v1/decisions?")) {
      const status = new URL(c.url, "http://localhost").searchParams.get("status");
      return { body: { items: status === "pending" ? pending() : [] } };
    }
    return { status: 404, body: {} };
  });
}

const mount = (path = "/") => renderAt(path, () => <Route path="/" component={(p: RouteSectionProps) => <Shell>{p.children}</Shell>}>
  <Route path="/" component={() => <p>Home</p>} />
  <Route path="/decisions" component={() => <p>Decisions page</p>} />
  <Route path="/decisions/:id" component={() => <p>Decision detail</p>} />
</Route>);

describe("Shell: decisions", () => {
  beforeEach(() => {
    resetDecisions();
    vi.stubGlobal("EventSource", MockEventSource);
  });
  afterEach(() => {
    resetDecisions();
    delete window.secretaryShell;
    delete document.documentElement.dataset.shell;
  });

  it("has no Decisions tab, but a button next to the bell with a badge for the pending count", async () => {
    serve(() => [decision("dc-000000000001", "Ship today?"), decision("dc-000000000002", "Restart p1?")]);
    mount();
    const button = await screen.findByRole("link", { name: "2 waiting for your answer" });
    expect(button).toHaveAttribute("href", "/decisions");
    expect(button.closest(".topbar-actions")).not.toBeNull();
    expect(button.nextElementSibling).toHaveClass("notif-bell");
    expect(button.querySelector(".notif-badge")).toHaveTextContent("2");
    expect(document.querySelectorAll('.tabbar a[href="/decisions"]')).toHaveLength(0);
    expect(document.querySelectorAll(".tabbar a")).toHaveLength(5);
    expect(document.querySelectorAll('.tabs-top a[href="/decisions"]')).toHaveLength(0);
  });

  it("hides the badge when nothing is pending", async () => {
    const calls = serve(() => []);
    mount();
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/decisions?status=pending&limit=50")).toBe(true));
    const button = screen.getByRole("link", { name: "Decisions" });
    expect(button).toHaveAttribute("href", "/decisions");
    expect(button.querySelector(".notif-badge")).toBeNull();
    expect(screen.queryByRole("status", { name: "Decision needed" })).toBeNull();
  });

  it("shows a banner once per pending decision; Review opens it and Dismiss hides it", async () => {
    serve(() => [decision("dc-000000000001", "Ship today?"), decision("dc-000000000002", "Restart p1?")]);
    mount();
    const banner = await screen.findByRole("status", { name: "Decision needed" });
    expect(banner).toHaveTextContent("Lucina needs your decision: Ship today?");
    expect(banner).toHaveTextContent("1 more waiting");
    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(await screen.findByText(/Restart p1\?/)).toBeInTheDocument();
    expect(screen.queryByText(/Ship today\?/)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Review" }));
    expect(await screen.findByText("Decision detail")).toBeInTheDocument();
    // Both were announced already: no banner comes back, and none shows on the decisions pages.
    expect(screen.queryByRole("status", { name: "Decision needed" })).toBeNull();
  });

  it("shows no banner on the decisions pages", async () => {
    const calls = serve(() => [decision("dc-000000000001", "Ship today?")]);
    mount("/decisions");
    await screen.findByText("Decisions page");
    await waitFor(() => expect(calls.some((c) => c.url.startsWith("/v1/decisions?"))).toBe(true));
    await screen.findByRole("link", { name: "1 waiting for your answer" });
    expect(screen.queryByRole("status", { name: "Decision needed" })).toBeNull();
  });

  it("shows the banner inside Secretary Code, where the bars are not rendered, and tells the rail the count", async () => {
    const setBadge = vi.fn();
    window.secretaryShell = { showCode: vi.fn(), setBadge };
    serve(() => [decision("dc-000000000001", "Ship today?")]);
    mount();
    expect(await screen.findByRole("status", { name: "Decision needed" })).toHaveTextContent("Ship today?");
    expect(document.querySelector(".tabbar")).toBeNull();
    expect(setBadge).toHaveBeenCalledWith("decisions", 1);
  });
});
