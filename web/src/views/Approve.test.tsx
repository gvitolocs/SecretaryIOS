import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it } from "vitest";
import { describeUserAgent } from "../lib/userAgent";
import { session, setSessionForTests } from "../state/session";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import Approve from "./Approve";

const MAC_SAFARI =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
const INFO = { ip: "203.0.113.7", userAgent: MAC_SAFARI, createdAt: "2026-10-27T18:00:00Z", expiresIn: 90 };

const open = (path = "/approve?c=abc") => renderAt(path, () => <Route path="/approve" component={Approve} />);

describe("Approve (QR sign-in)", () => {
  beforeEach(() => setSessionForTests({ status: "authed", deviceName: "iPhone" }));

  it("shows the asking device and approves it", async () => {
    const calls = stubFetch((c) => (c.method === "GET" ? { body: INFO } : { body: { status: "approved" } }));
    open();
    expect(await screen.findByRole("heading", { name: "Sign in on another device?" })).toBeInTheDocument();
    expect(await screen.findByTestId("approve-device")).toHaveTextContent("Safari on macOS");
    expect(screen.getByText("203.0.113.7")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    expect(await screen.findByText("Done. The other device is signed in.")).toBeInTheDocument();
    expect(calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET /v1/session/qr/info?c=abc",
      "POST /v1/session/qr/approve",
    ]);
    expect(calls[1]!.body).toEqual({ code: "abc", approve: true });
    expect(calls[1]!.headers["x-secretary-csrf"]).toBe("1");
  });

  it("denies", async () => {
    const calls = stubFetch((c) => (c.method === "GET" ? { body: INFO } : { body: { status: "denied" } }));
    open();
    fireEvent.click(await screen.findByRole("button", { name: "Deny" }));
    expect(await screen.findByText("Denied. The other device stays signed out.")).toBeInTheDocument();
    expect(calls[1]!.body).toEqual({ code: "abc", approve: false });
  });

  it("explains an expired or used code", async () => {
    stubFetch(() => ({ status: 404, body: { error: { code: "qr_not_found", message: "gone" } } }));
    open();
    expect(await screen.findByRole("alert")).toHaveTextContent("This code expired or was already used.");
    expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
  });

  it("explains a code answered meanwhile", async () => {
    stubFetch((c) =>
      c.method === "GET" ? { body: INFO } : { status: 409, body: { error: { code: "qr_already_decided", message: "x" } } },
    );
    open();
    fireEvent.click(await screen.findByRole("button", { name: "Approve" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("This code expired or was already used.");
  });

  it("needs a code in the link", async () => {
    const calls = stubFetch(() => ({ body: {} }));
    open("/approve");
    expect(await screen.findByRole("alert")).toHaveTextContent("This link has no sign-in code.");
    expect(calls).toHaveLength(0);
  });

  it("falls back to the login when the session is gone", async () => {
    stubFetch((c) =>
      c.url === "/v1/session" ? { body: { authenticated: false } } : { status: 401, body: { error: { code: "unauthorized", message: "x" } } },
    );
    open();
    await waitFor(() => expect(session().status).toBe("anon"));
  });
});

describe("describeUserAgent", () => {
  it.each([
    [MAC_SAFARI, "Safari", "macOS"],
    [
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36",
      "Chrome",
      "Windows",
    ],
    [
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0",
      "Edge",
      "Windows",
    ],
    ["Mozilla/5.0 (X11; Linux x86_64; rv:144.0) Gecko/20100101 Firefox/144.0", "Firefox", "Linux"],
    [
      "Mozilla/5.0 (iPhone; CPU iPhone OS 26_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.6 Mobile/15E148 Safari/604.1",
      "Safari",
      "iOS",
    ],
    [
      "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Mobile Safari/537.36",
      "Chrome",
      "Android",
    ],
    ["Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)", "Secretary app", "macOS"],
    ["curl/8.5.0", null, null],
  ])("%s", (ua, browser, os) => {
    expect(describeUserAgent(ua)).toEqual({ browser, os });
  });
});
