import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { session, setSessionForTests } from "../state/session";
import { stubFetch } from "../test/fetch";
import { AccountDialog } from "./AccountDialog";

function fill(current: string, next: string, confirm: string) {
  fireEvent.input(screen.getByLabelText("Current password"), { target: { value: current } });
  fireEvent.input(screen.getByLabelText("New password"), { target: { value: next } });
  fireEvent.input(screen.getByLabelText("Confirm new password"), { target: { value: confirm } });
  fireEvent.click(screen.getByRole("button", { name: "Change password" }));
}

describe("AccountDialog", () => {
  beforeEach(() => setSessionForTests({ status: "authed", deviceName: "Browser" }));

  it("validates length and confirmation before sending", async () => {
    const calls = stubFetch(() => ({ body: { ok: true } }));
    render(() => <AccountDialog open onClose={vi.fn()} />);
    fill("dev-password-123", "short", "short");
    expect(await screen.findByRole("alert")).toHaveTextContent("at least 8 characters");
    fill("dev-password-123", "a-much-longer-pass", "a-much-longer-pasS");
    expect(await screen.findByRole("alert")).toHaveTextContent("don't match");
    expect(calls).toHaveLength(0);
  });

  it("changes the password with the CSRF header", async () => {
    const calls = stubFetch(() => ({ body: { changed: true, sessionsRevoked: 1 } }));
    render(() => <AccountDialog open onClose={vi.fn()} />);
    fill("dev-password-123", "correct horse battery", "correct horse battery");
    expect(await screen.findByText("Password changed.")).toBeInTheDocument();
    expect(calls).toHaveLength(1);
    expect(calls[0]).toMatchObject({
      method: "POST",
      url: "/v1/session/password",
      credentials: "same-origin",
      body: { currentPassword: "dev-password-123", newPassword: "correct horse battery" },
    });
    expect(calls[0]!.headers["x-secretary-csrf"]).toBe("1");
  });

  it("reports a wrong current password without signing out", async () => {
    stubFetch(() => ({ status: 403, body: { error: { code: "invalid_credentials", message: "the current password is wrong" } } }));
    render(() => <AccountDialog open onClose={vi.fn()} />);
    fill("not-the-password", "correct horse battery", "correct horse battery");
    expect(await screen.findByRole("alert")).toHaveTextContent("Your current password is incorrect.");
    await waitFor(() => expect(session().status).toBe("authed"));
  });
});
