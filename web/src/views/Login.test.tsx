import { fireEvent, render, screen } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it } from "vitest";
import { session, setSessionForTests } from "../state/session";
import { stubFetch } from "../test/fetch";
import Login from "./Login";

function submitKey(value: string) {
  const input = screen.getByLabelText("Access key") as HTMLInputElement;
  fireEvent.input(input, { target: { value } });
  fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
}

describe("Login", () => {
  beforeEach(() => setSessionForTests({ status: "anon" }));

  it("renders a password field with autocomplete off", () => {
    render(() => <Login />);
    const input = screen.getByLabelText("Access key");
    expect(input).toHaveAttribute("type", "password");
    expect(input).toHaveAttribute("autocomplete", "off");
  });

  it("explains a rejected key (401)", async () => {
    stubFetch(() => ({ status: 401, body: { error: { code: "invalid_token", message: "Invalid" } } }));
    render(() => <Login />);
    submitKey("nope");
    expect(await screen.findByRole("alert")).toHaveTextContent("That access key wasn't accepted");
    expect(session().status).toBe("anon");
  });

  it("explains rate limiting (429) with the wait time", async () => {
    stubFetch(() => ({
      status: 429,
      body: { error: { code: "rate_limited", message: "Too many" } },
      headers: { "Retry-After": "30" },
    }));
    render(() => <Login />);
    submitKey("again");
    expect(await screen.findByRole("alert")).toHaveTextContent("Too many attempts. Please wait 30 seconds");
  });

  it("signs in and re-reads the session", async () => {
    const calls = stubFetch((c) =>
      c.method === "POST" ? { body: { authenticated: true } } : { body: { authenticated: true, deviceName: "iPhone" } },
    );
    render(() => <Login />);
    submitKey("dev");
    await screen.findByRole("button", { name: "Sign in" });
    await new Promise((r) => setTimeout(r, 0));
    expect(calls.map((c) => `${c.method} ${c.url}`)).toEqual(["POST /v1/session", "GET /v1/session"]);
    expect(calls[0]!.body).toEqual({ token: "dev" });
    expect(session()).toEqual({ status: "authed", deviceName: "iPhone" });
  });
});
