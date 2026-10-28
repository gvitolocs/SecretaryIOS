import { Route, type RouteSectionProps } from "@solidjs/router";
import { fireEvent, screen } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ApprovalPopup } from "../components/ApprovalPopup";
import { approvalJson, NOW } from "../test/approvals";
import { stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
import ApprovalDetail from "./ApprovalDetail";
import Approvals from "./Approvals";

async function settle(ms = 0) {
  await vi.advanceTimersByTimeAsync(ms);
  await vi.advanceTimersByTimeAsync(0);
}
async function until(check: () => void, tries = 50): Promise<void> {
  for (let i = 0; ; i++) {
    try {
      check();
      return;
    } catch (e) {
      if (i >= tries) throw e;
      await settle();
    }
  }
}

const routes = () => (
  <>
    <Route path="/approvals" component={Approvals} />
    <Route path="/approvals/:id" component={ApprovalDetail} />
  </>
);

describe("/approvals/:id (the Web Push link)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(NOW);
  });

  it("renders one approval and answers it", async () => {
    const calls = stubFetch((c: Call) => {
      if (c.method === "GET" && c.url === "/v1/approvals/apr_9") return { body: { ...approvalJson("apr_9"), status: "pending" } };
      if (c.method === "POST") return { body: { status: "approved" } };
      return { status: 404, body: {} };
    });
    renderAt("/approvals/apr_9", routes);
    await until(() => screen.getByRole("heading", { name: "Restart the tunnel (apr_9)" }));
    expect(screen.getByLabelText("Exact details")).toHaveTextContent("systemctl restart");
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await until(() => screen.getByText("Approved."));
    expect(calls.find((c) => c.method === "POST")!.url).toBe("/v1/approvals/apr_9/decide");
    expect(calls.find((c) => c.method === "POST")!.headers["x-secretary-csrf"]).toBe("1");
  });

  it("says so for an unknown id", async () => {
    stubFetch(() => ({ status: 404, body: { error: { code: "approval_not_found", message: "x" } } }));
    renderAt("/approvals/apr_nope", routes);
    await until(() => expect(screen.getByRole("alert")).toHaveTextContent("This request doesn't exist"));
    expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
  });

  it("says who answered one that is already decided", async () => {
    stubFetch(() => ({ body: { ...approvalJson("apr_9"), status: "approved", decidedBy: "iPhone" } }));
    renderAt("/approvals/apr_9", routes);
    await until(() => screen.getByText("This request was approved."));
    expect(screen.getByText("Answered on iPhone.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
  });

  it("says so for an expired one", async () => {
    stubFetch(() => ({ body: { ...approvalJson("apr_9"), status: "expired" } }));
    renderAt("/approvals/apr_9", routes);
    await until(() => screen.getByText("This request expired."));
    expect(screen.queryByRole("button", { name: "Deny" })).toBeNull();
  });

  it("does not offer Approve for a sensitive one", async () => {
    stubFetch(() => ({ body: { ...approvalJson("apr_9", { sensitive: true }), status: "pending" } }));
    renderAt("/approvals/apr_9", routes);
    await until(() => screen.getByText("Approve on your iPhone with Face ID"));
    expect(screen.getByRole("button", { name: "Approve" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Deny" })).toBeEnabled();
  });

  it("offers a retry when the server fails", async () => {
    let fail = true;
    stubFetch(() => (fail ? { status: 500, body: {} } : { body: { ...approvalJson("apr_9"), status: "pending" } }));
    renderAt("/approvals/apr_9", routes);
    await until(() => expect(screen.getByRole("alert")).toHaveTextContent("Couldn't load this request."));
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await until(() => screen.getByRole("heading", { name: "Restart the tunnel (apr_9)" }));
  });
});

describe("/approvals", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(NOW);
  });

  it("lists the pending requests", async () => {
    stubFetch((c) => (c.url === "/v1/approvals?status=pending" ? { body: [approvalJson("apr_1"), approvalJson("apr_2")] } : { status: 404, body: {} }));
    renderAt("/approvals", routes);
    await until(() => screen.getByRole("heading", { name: "Restart the tunnel (apr_1)" }));
    expect(screen.getByRole("heading", { name: "Restart the tunnel (apr_2)" })).toBeInTheDocument();
  });

  it("says when nothing is waiting, even if the server has no approvals yet", async () => {
    stubFetch(() => ({ status: 404, body: {} }));
    renderAt("/approvals", routes);
    await until(() => screen.getByText("Nothing is waiting for you."));
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("the pop-up steps aside on these pages", async () => {
    stubFetch((c) => (c.url === "/v1/approvals?status=pending" ? { body: [approvalJson("apr_1")] } : { status: 404, body: {} }));
    renderAt("/approvals", () => (
      <Route
        path="*"
        component={(p: RouteSectionProps) => (
          <>
            <ApprovalPopup />
            {p.children}
          </>
        )}
      >
        {routes()}
      </Route>
    ));
    await until(() => screen.getByRole("heading", { name: "Restart the tunnel (apr_1)" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(screen.getAllByRole("heading", { name: "Restart the tunnel (apr_1)" })).toHaveLength(1);
  });
});
