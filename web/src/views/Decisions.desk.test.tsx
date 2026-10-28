import { Route } from "@solidjs/router";
import { fireEvent, screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { stubWide } from "../test/wide";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import Decisions from "./Decisions";
import DecisionDetail from "./DecisionDetail";
beforeEach(() => stubWide());
afterEach(() => vi.unstubAllGlobals());
const routes = () => <><Route path="/decisions" component={Decisions} /><Route path="/decisions/:id" component={DecisionDetail} /></>;
function frame(container: HTMLElement) {
  expect(screen.getByRole("heading", { level: 1, name: "Decisions" })).toBeInTheDocument();
  expect(container.querySelectorAll(".pg-cta").length).toBeLessThanOrEqual(1);
  expect(container.querySelector(".apv-page-title, .pg-seg, .apv-page")).toBeNull();
  expect(container.querySelector(".pg-grid--single")).toBeInTheDocument();
}
it("keeps the header through loading and renders the empty list card", async () => {
  stubFetch(() => ({ body: { items: [] } }));
  const { container } = renderAt("/decisions", routes);
  frame(container);
  await screen.findByText("Nothing is waiting for you.");
  expect(screen.getByRole("region", { name: "Decisions" }).querySelector(".pg-empty")).toBeInTheDocument();
});
it("renders a list row with its status and retryable error", async () => {
  let fail = true;
  stubFetch(c => fail ? { status: 500, body: {} } : { body: { items: c.url.includes("status=pending") ? [{ decision_id: "dc-1", question: "Ship today?", asking_client: "agent", asking_session: null, created_at: "2026-10-28T08:00:00Z", impact: "low", urgency: "normal", status: "pending" }] : [] } });
  const { container } = renderAt("/decisions", routes);
  expect(await screen.findByRole("alert")).toHaveClass("pg-error");
  fail = false;
  fireEvent.click(screen.getByRole("button", { name: "Try again" }));
  await screen.findByRole("link", { name: "Ship today?" });
  expect(screen.getByRole("list", { name: "Decisions" }).querySelectorAll(".pg-row")).toHaveLength(1);
  expect(screen.getByText("Pending")).toBeInTheDocument();
  frame(container);
});
it("keeps the parent header and back link on a failed detail", async () => {
  stubFetch(() => ({ status: 500, body: {} }));
  const { container } = renderAt("/decisions/dc-1", routes);
  await screen.findByRole("alert");
  frame(container);
  expect(screen.getByRole("link", { name: "All decisions" })).toHaveClass("pg-back");
});

it("renders one detail heading, its evidence and quiet working answers", async () => {
  let answered = false;
  const calls = stubFetch(c => {
    if (c.method === "POST") { answered = true; return { body: { reply: "OK", job: null } }; }
    return { body: { decision_id: "dc-1", question: "Ship today?", asking_client: "agent", asking_session: null,
      created_at: "2026-10-28T08:00:00Z", impact: "low", urgency: "normal", status: answered ? "decided" : "pending",
      reason: "Review the release.", reversible: true, options: ["ship", "hold"], recommendation: "hold",
      chosen_option: answered ? "hold" : null, decided_by: answered ? "owner" : null,
      evidence: [{ tier: 1, kind: "record", text: "The release is ready." }] } };
  });
  const { container } = renderAt("/decisions/dc-1", routes);
  await screen.findByRole("button", { name: "hold (recommended)" });
  frame(container);
  expect(screen.getAllByRole("heading", { name: "Ship today?" })).toHaveLength(1);
  expect(screen.getByRole("region", { name: "Ship today?" })).toHaveClass("pg-card");
  expect(screen.getByText("The release is ready.")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "hold (recommended)" }));
  await screen.findByText("Answered by owner");
  expect(calls.find(c => c.method === "POST")?.body).toMatchObject({ message: "Decision dc-1: 2. hold" });
});
