import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { session, setSessionForTests } from "../state/session";
import { stubFetch } from "../test/fetch";
import Login from "./Login";

function fill(email: string, password: string) {
  fireEvent.input(screen.getByLabelText("Email"), { target: { value: email } });
  fireEvent.input(screen.getByLabelText("Password"), { target: { value: password } });
  fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
}

describe("Login", () => {
  beforeEach(() => setSessionForTests({ status: "anon" }));
  afterEach(() => vi.useRealTimers());

  it("uses password-manager friendly fields and a show-password toggle", () => {
    render(() => <Login />);
    const email = screen.getByLabelText("Email");
    const password = screen.getByLabelText("Password");
    expect(email).toHaveAttribute("type", "email");
    expect(email).toHaveAttribute("autocomplete", "username");
    expect(password).toHaveAttribute("type", "password");
    expect(password).toHaveAttribute("autocomplete", "current-password");
    expect(screen.queryByLabelText(/access key/i)).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Show" }));
    expect(password).toHaveAttribute("type", "text");
    fireEvent.click(screen.getByRole("button", { name: "Hide" }));
    expect(password).toHaveAttribute("type", "password");
  });

  it("signs in with email and password and re-reads the session", async () => {
    const calls = stubFetch((c) =>
      c.method === "POST" ? { body: { authenticated: true } } : { body: { authenticated: true, deviceName: "Browser" } },
    );
    render(() => <Login />);
    fill("owner@example.test", "dev-password-123");
    await waitFor(() => expect(session()).toEqual({ status: "authed", deviceName: "Browser" }));
    expect(calls.map((c) => `${c.method} ${c.url}`)).toEqual(["POST /v1/session", "GET /v1/session"]);
    expect(calls[0]!.body).toEqual({ email: "owner@example.test", password: "dev-password-123" });
  });

  it("shows one generic message for wrong credentials (401)", async () => {
    stubFetch(() => ({ status: 401, body: { error: { code: "invalid_credentials", message: "no such user" } } }));
    render(() => <Login />);
    fill("owner@example.test", "wrong-password");
    expect(await screen.findByRole("alert")).toHaveTextContent("Email or password is incorrect");
    expect(screen.getByRole("alert")).not.toHaveTextContent("no such user");
    expect(session().status).toBe("anon");
  });

  it("counts down an account lock and re-enables sign-in", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
    const calls = stubFetch(() => ({
      status: 429,
      body: { error: { code: "account_locked", message: "Locked" } },
      headers: { "Retry-After": "3" },
    }));
    render(() => <Login />);
    fill("owner@example.test", "wrong-password");

    expect(await screen.findByRole("alert")).toHaveTextContent("Sign-in is locked after too many attempts. Try again in 0:03.");
    expect(screen.getByRole("button", { name: "Sign in" })).toBeDisabled();

    vi.advanceTimersByTime(1_000);
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("Try again in 0:02."));
    vi.advanceTimersByTime(2_000);
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(screen.getByRole("button", { name: "Sign in" })).toBeEnabled();
    expect(calls).toHaveLength(1);
  });

  it("says rate limited for rate_limited", async () => {
    stubFetch(() => ({
      status: 429,
      body: { error: { code: "rate_limited", message: "Too many" } },
      headers: { "Retry-After": "75" },
    }));
    render(() => <Login />);
    fill("owner@example.test", "x");
    expect(await screen.findByRole("alert")).toHaveTextContent("Too many attempts. Try again in 1:15.");
  });
});
