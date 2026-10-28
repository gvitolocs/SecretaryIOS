import { Route } from "@solidjs/router";
import { fireEvent, screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { stubFetch, type Handler } from "../test/fetch";
import { approvalJson, NOW, stubApprover } from "../test/approvals";
import { renderAt } from "../test/router";
import { ApprovalPopup, FLASH_MS } from "./ApprovalPopup";

// Inside Secretary Code a Mac with a Touch ID key approves sensitive requests (window.secretaryApprover).

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

function server(items: () => Record<string, unknown>[]) {
  const handler: Handler = (c) => {
    if (c.method === "GET" && c.url === "/v1/approvals?status=pending") return { body: items() };
    if (c.method === "POST" && c.url.endsWith("/decide")) return { body: { status: "approved" } };
    return { status: 404, body: {} };
  };
  return stubFetch(handler);
}

const open = () => renderAt("/", () => <Route path="*" component={ApprovalPopup} />);
const sensitive = (id = "apr_s") => approvalJson(id, { sensitive: true, risk: "high", action: "root.exec" });

describe("approval card with Touch ID on this Mac", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(NOW);
  });
  afterEach(() => {
    delete window.secretaryApprover;
  });

  it("without the bridge a sensitive request still only says Face ID", async () => {
    server(() => [sensitive()]);
    open();
    await until(() => screen.getByRole("alertdialog"));
    expect(screen.getByRole("button", { name: "Approve" })).toBeDisabled();
    expect(screen.getByText("Approve on your iPhone with Face ID")).toBeInTheDocument();
    expect(screen.queryByText("Set up Touch ID on this Mac")).toBeNull();
  });

  it("enables Approve and approves with Touch ID through the bridge, not the web decide", async () => {
    const bridge = stubApprover();
    let answered = false;
    const calls = server(() => (answered ? [] : [sensitive()]));
    bridge.approve.mockImplementation(async () => {
      answered = true;
      return { ok: true, status: "approved" };
    });
    open();
    await until(() => expect(screen.getByRole("button", { name: "Approve" })).toBeEnabled());
    expect(screen.getByText("Approve with Touch ID on this Mac, or on your iPhone with Face ID")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await until(() => screen.getByText("Approved."));
    expect(bridge.approve).toHaveBeenCalledWith("apr_s");
    expect(calls.some((c) => c.method === "POST")).toBe(false);
    await settle(FLASH_MS + 10);
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("Deny and not-sensitive requests keep using the web path", async () => {
    const bridge = stubApprover();
    const calls = server(() => [approvalJson("apr_1")]);
    open();
    await until(() => expect(screen.getByRole("button", { name: "Approve" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await until(() => screen.getByText("Approved."));
    expect(bridge.approve).not.toHaveBeenCalled();
    expect(calls.find((c) => c.method === "POST")!.url).toBe("/v1/approvals/apr_1/decide");
  });

  it("denying a sensitive request goes to the server, not the bridge", async () => {
    const bridge = stubApprover();
    const calls = server(() => [sensitive()]);
    open();
    await until(() => expect(screen.getByRole("button", { name: "Approve" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "Deny" }));
    await until(() => expect(calls.some((c) => c.method === "POST")).toBe(true));
    expect(calls.find((c) => c.method === "POST")!.body).toMatchObject({ decision: "deny" });
    expect(bridge.approve).not.toHaveBeenCalled();
  });

  it("stays on the card, silently, when the owner cancels Touch ID", async () => {
    const bridge = stubApprover({ approve: vi.fn(async () => ({ ok: false as const, error: "cancelled" as const })) });
    server(() => [sensitive()]);
    open();
    await until(() => expect(screen.getByRole("button", { name: "Approve" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await until(() => expect(bridge.approve).toHaveBeenCalled());
    await settle();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByRole("button", { name: "Approve" })).toBeEnabled();
    expect(screen.queryByText("Approved.")).toBeNull();
  });

  it.each([
    ["decided", "Already answered on another device."],
    ["expired", "This request expired."],
    ["notPending", "This request is gone."],
  ] as const)("closes the card like the web path when the bridge says %s", async (error, text) => {
    stubApprover({ approve: vi.fn(async () => ({ ok: false as const, error })) });
    server(() => [sensitive()]);
    open();
    await until(() => expect(screen.getByRole("button", { name: "Approve" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await until(() => screen.getByText(text));
  });

  it.each([
    ["changed", "This request changed. Check the details and try again."],
    ["network", "Couldn't send your answer. Try again."],
    ["server", "Couldn't send your answer. Try again."],
    ["unavailable", "Touch ID isn't available on this Mac right now."],
  ] as const)("shows a note and keeps the card for %s", async (error, text) => {
    stubApprover({ approve: vi.fn(async () => ({ ok: false as const, error })) });
    server(() => [sensitive()]);
    open();
    await until(() => expect(screen.getByRole("button", { name: "Approve" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await until(() => expect(screen.getByRole("alert")).toHaveTextContent(text));
  });

  it("needsSetup points to the settings and turns the card into the setup state", async () => {
    let hasKey = true;
    stubApprover({
      status: vi.fn(async () => ({ available: true, hasKey })),
      approve: vi.fn(async () => {
        hasKey = false;
        return { ok: false as const, error: "needsSetup" as const };
      }),
    });
    server(() => [sensitive()]);
    open();
    await until(() => expect(screen.getByRole("button", { name: "Approve" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await until(() => expect(screen.getByRole("alert")).toHaveTextContent("Set it up in Settings → Approvals."));
    await until(() => expect(screen.getByRole("button", { name: "Approve" })).toBeDisabled());
    expect(screen.getByRole("link", { name: "Set up Touch ID on this Mac" })).toBeInTheDocument();
  });

  it("keeps Approve disabled and links to the settings while this Mac has no key", async () => {
    const bridge = stubApprover({ status: vi.fn(async () => ({ available: true, hasKey: false })) });
    const calls = server(() => [sensitive()]);
    open();
    await until(() => screen.getByRole("link", { name: "Set up Touch ID on this Mac" }));
    const approve = screen.getByRole("button", { name: "Approve" });
    expect(approve).toBeDisabled();
    expect(screen.getByRole("link", { name: "Set up Touch ID on this Mac" })).toHaveAttribute("href", "/account?section=approvals");
    expect(screen.getByText(/Approve on your iPhone with Face ID/)).toBeInTheDocument();
    fireEvent.click(approve);
    await settle();
    expect(bridge.approve).not.toHaveBeenCalled();
    expect(calls.some((c) => c.method === "POST")).toBe(false);
  });

  it("does not offer Touch ID while the Mac cannot do it", async () => {
    stubApprover({ status: vi.fn(async () => ({ available: false, hasKey: false })) });
    server(() => [sensitive()]);
    open();
    await until(() => screen.getByRole("alertdialog"));
    await settle();
    expect(screen.getByRole("button", { name: "Approve" })).toBeDisabled();
    expect(screen.getByText("Approve on your iPhone with Face ID")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Set up Touch ID on this Mac" })).toBeNull();
  });
});
