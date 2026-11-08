import { Route } from "@solidjs/router";
import { fireEvent, screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { stubWide } from "../test/wide";
import { approvalJson } from "../test/approvals";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { resetApprovals } from "../state/approvals";
import Approvals from "./Approvals";
import ApprovalDetail from "./ApprovalDetail";
beforeEach(() => { stubWide(); resetApprovals(); });
afterEach(() => vi.unstubAllGlobals());
const routes = () => <><Route path="/approvals" component={Approvals} /><Route path="/approvals/:id" component={ApprovalDetail} /></>;
function frame(container: HTMLElement) {
  expect(screen.getByRole("heading", { level: 1, name: "Approvals" })).toBeInTheDocument();
  expect(container.querySelectorAll(".pg-cta").length).toBeLessThanOrEqual(1);
  expect(container.querySelector(".apv-page-title, .pg-seg, .apv-page")).toBeNull();
  expect(container.querySelector(".pg-grid--single")).toBeInTheDocument();
}
const request = () => ({ ...approvalJson("apr_1"), expiresAt: new Date(Date.now() + 600000).toISOString() });
it("renders pending summaries as links in one list card", async () => {
  stubFetch(() => ({ body: [request()] }));
  const { container } = renderAt("/approvals", routes);
  frame(container);
  await screen.findByRole("link", { name: "Restart the tunnel (apr_1)" });
  expect(screen.getByRole("list", { name: "Approvals" }).querySelectorAll(".pg-row")).toHaveLength(1);
  expect(screen.getByText("Pending")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
});
it("shows an empty card and retries a list failure", async () => {
  let fail = true;
  stubFetch(() => fail ? { status: 500, body: {} } : { body: [] });
  const { container } = renderAt("/approvals", routes);
  expect(await screen.findByRole("alert")).toHaveClass("pg-error");
  fail = false;
  fireEvent.click(screen.getByRole("button", { name: "Try again" }));
  await screen.findByText("Nothing is waiting for you.");
  expect(screen.getByRole("region", { name: "Approvals" }).querySelector(".pg-empty")).toBeInTheDocument();
  frame(container);
});
it("keeps details and confirms denial before sending", async () => {
  const calls = stubFetch(c => c.method === "POST" ? { body: { status: "denied" } } : { body: { ...request(), status: "pending" } });
  const { container } = renderAt("/approvals/apr_1", routes);
  await screen.findByRole("heading", { name: "Restart the tunnel (apr_1)" });
  frame(container);
  expect(screen.getByRole("region", { name: "Restart the tunnel (apr_1)" })).toHaveClass("pg-card");
  expect(screen.getByLabelText("Exact details")).toHaveTextContent("systemctl restart");
  expect(screen.getByRole("link", { name: "All approvals" })).toHaveClass("pg-back");
  fireEvent.click(screen.getByRole("button", { name: "Deny" }));
  expect(calls.filter(c => c.method === "POST")).toHaveLength(0);
  expect(screen.getByRole("button", { name: "Cancel" })).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Deny" }));
  await screen.findByText("Denied.");
  expect(calls.find(c => c.method === "POST")?.body).toMatchObject({ decision: "deny" });
});
