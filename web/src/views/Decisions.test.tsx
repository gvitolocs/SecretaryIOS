import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import type { Decision } from "../api/decisions";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import DecisionDetail from "./DecisionDetail";
import Decisions from "./Decisions";
import Plugins from "./Plugins";

const ID = "dc-0123456789ab";
const decision = (overrides: Partial<Decision> = {}): Decision => ({
  decision_id: ID, status: "pending", decision: null, chosen_option: null, chosen_index: null,
  reason: "This needs your go-ahead.", decided_by: null, confidence: null, asked_owner: true, owner_only: true,
  question: "Ship today?", options: ["ship", "hold"], recommendation: "hold", impact: "high", reversible: true,
  urgency: "normal", topic: "release", asking_client: "agent", asking_session: "session-1",
  created_at: "2026-10-28T08:00:00Z", decided_at: null, expires_at: null, basis: null,
  evidence: [1, 2, 3, 4].map(tier => ({ kind: "record", text: `Evidence ${tier}`, tier })), ...overrides,
});
const routes = () => <>
  <Route path="/decisions" component={Decisions} />
  <Route path="/decisions/:id" component={DecisionDetail} />
  <Route path="/plugins" component={Plugins} />
</>;

function detailApi(opts: { failSend?: boolean; failRefresh?: boolean } = {}) {
  let answered = false;
  let reads = 0;
  const calls = stubFetch(c => {
    if (c.method === "GET" && c.url.includes("?")) return { body: { items: [] } }; // the pending list the nav badge reloads
    if (c.method === "GET") {
      reads++;
      if (answered && opts.failRefresh) return { status: 500, body: {} };
      return { body: decision(answered ? { status: "decided", chosen_option: "hold", chosen_index: 2, decided_by: "owner" } : {}) };
    }
    if (c.url === "/v1/chat") {
      if (opts.failSend) return { status: 500, body: {} };
      answered = true;
      return { body: { reply: "Ok, agent gets hold", job: null } };
    }
    return { status: 404, body: {} };
  });
  return { calls, reads: () => reads };
}

describe("Decisions", () => {
  it("lists pending first and the latest 20 settled decisions collapsed under Earlier", async () => {
    stubFetch(c => {
      const status = new URL(c.url, "http://localhost").searchParams.get("status");
      return { body: { items: status === "pending" ? [decision()] : status === "decided" ? Array.from({ length: 20 }, (_, i) => decision({ decision_id: `dc-${String(i).padStart(12, "0")}`, status: "decided", question: `Earlier ${i}`, created_at: `2026-10-${String(27 - i).padStart(2, "0")}T08:00:00Z` })) : [decision({ decision_id: "dc-ffffffffffff", status: "expired", question: "Old expired", created_at: "2025-01-01T00:00:00Z" })] } };
    });
    const { container } = renderAt("/decisions", routes);
    await screen.findByRole("heading", { name: "Ship today?" });
    const links = container.querySelectorAll('a[href^="/decisions/"]');
    expect(links).toHaveLength(21);
    expect(links[0]).toHaveTextContent("Ship today?");
    expect(links[0]).toHaveTextContent("agent (session-1)");
    expect(links[0]).toHaveTextContent("High impact");
    expect(links[0]).toHaveTextContent("Normal urgency");
    expect(links[0]!.querySelector("time")).toHaveAttribute("datetime", "2026-10-28T08:00:00Z");
    const earlier = screen.getByText("Earlier", { selector: "summary" }).closest("details")!;
    expect(earlier.open).toBe(false);
    expect(earlier.querySelectorAll("li")).toHaveLength(20);
    expect(screen.queryByText("Old expired")).toBeNull();
  });

  it("shows what loaded when a history list fails (the server once answered 400 to status=withdrawn)", async () => {
    stubFetch(c => {
      const status = new URL(c.url, "http://localhost").searchParams.get("status");
      if (status === "withdrawn") return { status: 400, body: { error: { code: "bad_request", message: "no" } } };
      return { body: { items: status === "pending" ? [decision()] : status === "decided" ? [decision({ decision_id: "dc-aaaaaaaaaaaa", status: "decided", question: "Was shipped" })] : [] } };
    });
    const { container } = renderAt("/decisions", routes);
    await screen.findByRole("heading", { name: "Ship today?" });
    expect(screen.queryByRole("alert")).toBeNull();
    expect(container.querySelectorAll('a[href^="/decisions/"]')).toHaveLength(2);
    expect(screen.getByText("Earlier", { selector: "summary" })).toBeInTheDocument();
  });

  it("shows the error only when the pending list itself fails, and retries", async () => {
    let fail = true;
    stubFetch(c => {
      const status = new URL(c.url, "http://localhost").searchParams.get("status");
      if (status === "pending" && fail) return { status: 500, body: {} };
      return { body: { items: status === "pending" ? [decision()] : [] } };
    });
    renderAt("/decisions", routes);
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't load decisions.");
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByRole("heading", { name: "Ship today?" });
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("offers Decisions alongside Approvals in Plugins", async () => {
    stubFetch(() => ({ body: { items: [] } }));
    renderAt("/plugins", routes);
    expect(screen.getByRole("button", { name: "Approvals" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /^Decisions/ }));
    await screen.findByRole("heading", { name: "Decisions" });
  });

  it("renders options, recommendation and collapsed evidence; sends the exact second option through web chat", async () => {
    const { calls, reads } = detailApi();
    renderAt(`/decisions/${ID}`, routes);
    const hold = await screen.findByRole("button", { name: "hold (recommended)" });
    expect(screen.getByRole("button", { name: "ship" })).toBeInTheDocument();
    expect(screen.getByText("What Lucina looked at").closest("details")!.open).toBe(false);
    for (const label of ["his words", "his actions", "Lucina's earlier decision", "agent record"]) expect(screen.getByText(label)).toBeInTheDocument();
    fireEvent.click(hold);
    await screen.findByText("Answered by owner");
    const post = calls.find(c => c.method === "POST")!;
    expect(post.url).toBe("/v1/chat");
    expect(post.body).toMatchObject({ message: `Decision ${ID}: 2. hold`, sessionId: expect.any(String), clientMessageId: expect.any(String) });
    expect(post.headers["x-secretary-client"]).toBe("web");
    expect(post.headers["x-secretary-csrf"]).toBe("1");
    expect(reads()).toBe(2);
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("sends a free-text answer exactly", async () => {
    const { calls } = detailApi();
    renderAt(`/decisions/${ID}`, routes);
    fireEvent.input(await screen.findByLabelText("Your answer"), { target: { value: "wait a day" } });
    fireEvent.click(screen.getByRole("button", { name: "Answer in my words" }));
    await screen.findByText("Answered by owner");
    expect(calls.find(c => c.method === "POST")!.body).toMatchObject({ message: `Decision ${ID}: wait a day` });
  });

  it.each(["decided", "expired", "withdrawn"] as const)("shows the %s outcome with no answer buttons", async status => {
    stubFetch(() => ({ body: decision({ status, chosen_option: "hold", decided_by: "owner", reason: "Wait for review." }) }));
    renderAt(`/decisions/${ID}`, routes);
    await screen.findByText("Answered by owner");
    expect(screen.getByText("hold")).toBeInTheDocument();
    expect(screen.getByText("Wait for review.")).toBeInTheDocument();
    expect(screen.queryByRole("button")).toBeNull();
    expect(screen.queryByRole("textbox")).toBeNull();
  });

  it("shows a clear missing decision state for 404", async () => {
    stubFetch(() => ({ status: 404, body: { error: { code: "decision_not_found" } } }));
    renderAt(`/decisions/${ID}`, routes);
    expect(await screen.findByRole("alert")).toHaveTextContent("No such decision.");
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("keeps the answer available when sending fails", async () => {
    detailApi({ failSend: true });
    renderAt(`/decisions/${ID}`, routes);
    fireEvent.click(await screen.findByRole("button", { name: "ship" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't send your answer.");
    expect(screen.getByRole("button", { name: "ship" })).toBeEnabled();
  });

  it("does not resend an accepted answer when refreshing fails", async () => {
    const opts = { failRefresh: true };
    const { calls } = detailApi(opts);
    renderAt(`/decisions/${ID}`, routes);
    fireEvent.click(await screen.findByRole("button", { name: "ship" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Your answer was sent.");
    expect(screen.getByRole("button", { name: "ship" })).toBeDisabled();
    expect(calls.filter(c => c.method === "POST")).toHaveLength(1);
    opts.failRefresh = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByText("Answered by owner");
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("retries a failed detail load", async () => {
    let fail = true;
    stubFetch(() => fail ? { status: 500, body: {} } : { body: decision() });
    renderAt(`/decisions/${ID}`, routes);
    await screen.findByRole("alert");
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(screen.getByRole("heading", { name: "Ship today?" })).toBeInTheDocument());
  });
});
