import { Route } from "@solidjs/router";
import { fireEvent, screen } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { APPROVAL_POLL_MS } from "../state/approvals";
import { stubFetch, type Call, type Handler } from "../test/fetch";
import { approvalJson, NOW } from "../test/approvals";
import { renderAt } from "../test/router";
import { ApprovalPopup, FLASH_MS } from "./ApprovalPopup";

/** Let stubbed fetches and the promise chains behind them settle (all timers are faked). */
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

type Item = ReturnType<typeof approvalJson>;

/** A fake server: the pending list, and a scripted answer for decide. */
function server(opts: { list?: () => Item[] | { status: number }; decide?: (c: Call) => { status?: number; body?: unknown } } = {}) {
  const handler: Handler = (c) => {
    if (c.method === "GET" && c.url === "/v1/approvals?status=pending") {
      const r = opts.list?.() ?? [];
      return Array.isArray(r) ? { body: r } : { status: r.status, body: { error: { code: "not_found", message: "no" } } };
    }
    if (c.method === "POST" && c.url.endsWith("/decide")) return opts.decide?.(c) ?? { body: { status: "approved" } };
    return { status: 404, body: {} };
  };
  const calls = stubFetch(handler);
  const lists = () => calls.filter((c) => c.method === "GET" && c.url.startsWith("/v1/approvals?")).length;
  return { calls, lists };
}

const open = () => renderAt("/", () => <Route path="*" component={ApprovalPopup} />);
const err = (status: number, code: string) => ({ status, body: { error: { code, message: code } } });

describe("approval pop-up", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(NOW);
  });

  it("appears for a pending request and keeps polling every 5 s", async () => {
    const s = server({ list: () => [approvalJson("apr_1", { sensitive: false, risk: "high" })] });
    open();
    await until(() => screen.getByRole("alertdialog"));
    expect(screen.getByText("Lucina is asking for your OK")).toBeInTheDocument();
    expect(screen.getByText("ask_lucina from Claude Code")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Restart the tunnel (apr_1)" })).toBeInTheDocument();
    expect(screen.getByLabelText("Exact details")).toHaveTextContent("systemctl restart oracle-poko-tunnel");
    expect(screen.getByText("High risk")).toBeInTheDocument();
    expect(screen.getByTestId("apv-expiry")).toHaveTextContent("Expires in 5:00");
    expect(screen.getByRole("button", { name: "Approve" })).toBeEnabled();
    expect(screen.queryByText(/1 of/)).toBeNull();

    const first = s.lists();
    await settle(APPROVAL_POLL_MS);
    expect(s.lists()).toBe(first + 1);
    await settle(APPROVAL_POLL_MS);
    expect(s.lists()).toBe(first + 2);
    // The countdown ticks.
    expect(screen.getByTestId("apv-expiry")).toHaveTextContent("Expires in 4:50");
  });

  it("approve sends the decision and the payload hash with the CSRF header", async () => {
    let answered = false;
    const s = server({
      list: () => (answered ? [] : [approvalJson("apr_1")]),
      decide: () => {
        answered = true;
        return { body: { status: "approved" } };
      },
    });
    open();
    await until(() => screen.getByRole("button", { name: "Approve" }));
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await until(() => screen.getByText("Approved."));
    const post = s.calls.find((c) => c.method === "POST")!;
    expect(post.url).toBe("/v1/approvals/apr_1/decide");
    expect(post.body).toEqual({ decision: "approve", payloadHash: `sha256:${"apr_1".padEnd(64, "a")}` });
    expect(post.headers["x-secretary-csrf"]).toBe("1");
    // The short state goes away and nothing is left to ask.
    await settle(FLASH_MS + 10);
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("denies", async () => {
    const s = server({ list: () => [approvalJson("apr_1")], decide: () => ({ body: { status: "denied" } }) });
    open();
    await until(() => screen.getByRole("button", { name: "Deny" }));
    fireEvent.click(screen.getByRole("button", { name: "Deny" }));
    await until(() => screen.getByText("Denied."));
    expect(s.calls.find((c) => c.method === "POST")!.body).toMatchObject({ decision: "deny" });
  });

  it("cannot approve a sensitive request here, but can deny it", async () => {
    const s = server({
      list: () => [approvalJson("apr_s", { sensitive: true, risk: "high", action: "root.exec" })],
      decide: () => ({ body: { status: "denied" } }),
    });
    open();
    await until(() => screen.getByRole("alertdialog"));
    const approve = screen.getByRole("button", { name: "Approve" });
    expect(approve).toBeDisabled();
    expect(screen.getByText("Approve on your iPhone with Face ID")).toBeInTheDocument();
    expect(screen.getByText("Sensitive")).toBeInTheDocument();
    fireEvent.click(approve);
    await settle();
    expect(s.calls.some((c) => c.method === "POST")).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "Deny" }));
    await until(() => screen.getByText("Denied."));
    expect(s.calls.find((c) => c.method === "POST")!.body).toMatchObject({ decision: "deny" });
  });

  it.each([
    [404, "approval_not_found", "This request is gone."],
    [409, "approval_decided", "Already answered on another device."],
    [410, "approval_expired", "This request expired."],
  ])("says so when the server answers %i, and moves on", async (status, code, text) => {
    let gone = false;
    server({
      list: () => (gone ? [approvalJson("apr_2")] : [approvalJson("apr_1"), approvalJson("apr_2")]),
      decide: () => {
        gone = true;
        return err(status, code);
      },
    });
    open();
    await until(() => screen.getByRole("heading", { name: "Restart the tunnel (apr_1)" }));
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await until(() => screen.getByText(text));
    await settle(FLASH_MS + 10);
    await until(() => screen.getByRole("heading", { name: "Restart the tunnel (apr_2)" }));
  });

  it("shows the request as expired when its time runs out", async () => {
    server({ list: () => [approvalJson("apr_1", { ttl: 3 })] });
    open();
    await until(() => screen.getByTestId("apv-expiry"));
    expect(screen.getByTestId("apv-expiry")).toHaveTextContent("Expires in 0:03");
    await settle(3_000);
    await until(() => screen.getByText("This request expired."));
    await settle(FLASH_MS + 10);
    // It stays closed even though the (stale) list still has it.
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("shows several requests one at a time with their place in the queue", async () => {
    let remaining = ["apr_1", "apr_2", "apr_3"];
    server({
      list: () => remaining.map((id) => approvalJson(id)),
      decide: (c) => {
        remaining = remaining.filter((id) => !c.url.includes(id));
        return { body: { status: "approved" } };
      },
    });
    open();
    await until(() => screen.getByText("1 of 3"));
    expect(screen.getAllByRole("heading")).toHaveLength(1);
    expect(screen.getByRole("heading", { name: "Restart the tunnel (apr_1)" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await until(() => screen.getByText("Approved."));
    await settle(FLASH_MS + 10);
    await until(() => screen.getByText("1 of 2"));
    expect(screen.getByRole("heading", { name: "Restart the tunnel (apr_2)" })).toBeInTheDocument();
  });

  it("puts a request off with Later", async () => {
    server({ list: () => [approvalJson("apr_1"), approvalJson("apr_2")] });
    open();
    await until(() => screen.getByText("1 of 2"));
    fireEvent.click(screen.getByRole("button", { name: "Later" }));
    await until(() => screen.getByRole("heading", { name: "Restart the tunnel (apr_2)" }));
    // The next poll does not bring the first one back.
    await settle(APPROVAL_POLL_MS);
    expect(screen.getByRole("heading", { name: "Restart the tunnel (apr_2)" })).toBeInTheDocument();
    expect(screen.queryByText("1 of 2")).toBeNull();
  });

  it("does not poll while the page is hidden, and polls again when it is shown", async () => {
    const s = server({ list: () => [] });
    const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    open();
    await settle();
    expect(s.lists()).toBe(0);
    await settle(APPROVAL_POLL_MS * 3);
    expect(s.lists()).toBe(0);

    visibility.mockReturnValue("visible");
    document.dispatchEvent(new Event("visibilitychange"));
    await until(() => expect(s.lists()).toBe(1));
    await settle(APPROVAL_POLL_MS);
    expect(s.lists()).toBe(2);
  });

  it("stays quiet when the server doesn't have the routes yet", async () => {
    const s = server({ list: () => ({ status: 404 }) });
    open();
    await settle(APPROVAL_POLL_MS * 2);
    expect(s.lists()).toBeGreaterThanOrEqual(2);
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("keeps the open card when the next poll fails", async () => {
    let down = false;
    server({ list: () => (down ? { status: 500 } : [approvalJson("apr_1")]) });
    open();
    await until(() => screen.getByRole("alertdialog"));
    down = true;
    await settle(APPROVAL_POLL_MS);
    expect(screen.getByRole("alertdialog")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("does not rebuild an open card on every poll", async () => {
    server({ list: () => [approvalJson("apr_1")] });
    open();
    await until(() => screen.getByRole("alertdialog"));
    const details = screen.getByLabelText("Exact details");
    await settle(APPROVAL_POLL_MS * 2);
    expect(screen.getByLabelText("Exact details")).toBe(details);
  });

  it("shows even when Secretary Code hosts the page (no top or tab bar)", async () => {
    window.secretaryShell = { showCode: vi.fn() };
    try {
      server({ list: () => [approvalJson("apr_1")] });
      open();
      await until(() => screen.getByRole("alertdialog"));
    } finally {
      delete window.secretaryShell;
    }
  });
});
