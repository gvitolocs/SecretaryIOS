import { Route } from "@solidjs/router";
import { screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { installEventSource } from "../test/eventsource";
import { item, permission, serveAgents } from "../test/agentsFixtures";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import AgentsView from "./Agents";

const AGENTS = [
  item("a1", { title: "Fix the build", bucket: "needs_input", status: "running" }),
  item("a2", { title: "Write docs", bucket: "running", status: "running" }),
];

function renderAgents(path = "/agents") {
  return renderAt(path, () => <Route path="/agents/*rest" component={AgentsView} />);
}

describe("desktop Agents", () => {
  beforeEach(() => {
    installEventSource();
    stubWide();
  });
  afterEach(() => vi.unstubAllGlobals());

  it("uses the page header, section nav and split list on Runs", async () => {
    serveAgents({ items: AGENTS, absent: ["/search", "/schedules"] });
    renderAgents();
    expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent("Agents");
    await screen.findByRole("link", { name: "Fix the build" });
    expect(await screen.findByText(/1 running/)).toBeInTheDocument();
    expect(screen.getByText(/1 need permission/)).toBeInTheDocument();
    const nav = screen.getByRole("navigation", { name: "Sections" });
    expect(within(nav).getByRole("link", { name: "Runs" })).toHaveAttribute("aria-current", "page");
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(1);
    expect(document.querySelector(".ag-nav")).toBeNull();
    expect(await screen.findByRole("list", { name: "Agents" })).toBeInTheDocument();
    expect(screen.getByText("Pick an agent, or start a new run.")).toBeInTheDocument();
  });

  it("shows Permissions in the nav with a count badge label", async () => {
    serveAgents({ items: AGENTS, permissions: [permission()], absent: ["/search", "/schedules"] });
    renderAgents("/agents/permissions");
    const nav = await screen.findByRole("navigation", { name: "Sections" });
    await waitFor(() => expect(within(nav).getByRole("link", { name: "Permissions (1)" })).toBeInTheDocument());
    expect(within(nav).getByRole("link", { name: "Permissions (1)" })).toHaveAttribute("aria-current", "page");
  });

  it("shows an empty list message on Runs", async () => {
    serveAgents({ absent: ["/search", "/schedules"] });
    renderAgents();
    expect(await screen.findByText("No agents yet. Start a new run.")).toBeInTheDocument();
    await waitFor(() => expect(document.querySelector(".ag-rows")).toBeNull());
  });
});
