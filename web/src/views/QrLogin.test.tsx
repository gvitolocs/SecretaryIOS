import { fireEvent, render, screen } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { session, setSessionForTests } from "../state/session";
import { stubFetch, type Call } from "../test/fetch";
import Login from "./Login";
import QrLogin, { MAX_CODES, POLL_MS } from "./QrLogin";

type Poll = "pending" | "approved" | "denied" | "expired";

/** A fake server: numbered codes, a scripted poll answer, and the session after approval. */
function server(opts: { poll?: () => Poll; expiresIn?: number } = {}) {
  let n = 0;
  let approved = false;
  const calls = stubFetch((c: Call) => {
    if (c.method === "POST" && c.url === "/v1/session/qr") {
      n += 1;
      return { body: { code: `code${n}`, shortCode: `48291${n}`, pollToken: `tok${n}`, approveUrl: `https://x/approve?c=code${n}`, expiresIn: opts.expiresIn ?? 120 } };
    }
    if (c.url.startsWith("/v1/session/qr/poll")) {
      const status = opts.poll?.() ?? "pending";
      if (status === "approved") approved = true;
      return { body: { status } };
    }
    if (c.url === "/v1/session") return { body: approved ? { authenticated: true, deviceName: "web" } : { authenticated: false } };
    return { status: 404, body: {} };
  });
  const urls = () => calls.map((c) => `${c.method} ${c.url}`);
  return { calls, urls };
}

/** Let the stubbed fetches settle (setTimeout is not faked here). */
const flush = () => new Promise((r) => setTimeout(r, 5));
const img = () => screen.getByRole("img", { name: "Sign-in QR code" });
/** Retry `check` while the fetches settle. (waitFor polls with setInterval, which is faked here.) */
async function until(check: () => void, tries = 100): Promise<void> {
  for (let i = 0; ; i++) {
    try {
      check();
      return;
    } catch (e) {
      if (i >= tries) throw e;
      await flush();
    }
  }
}
/** Advance `ms` one poll interval at a time, letting each poll finish. */
async function run(ms: number): Promise<void> {
  await flush();
  for (let t = 0; t < ms; t += POLL_MS) {
    vi.advanceTimersByTime(POLL_MS);
    await flush();
    await flush();
  }
}
const image = async () => {
  await until(img);
  return img();
};

describe("QR sign-in", () => {
  beforeEach(() => {
    setSessionForTests({ status: "anon" });
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
  });

  it("shows the QR code next to the password form", async () => {
    server();
    render(() => <Login />);
    expect(await image()).toHaveAttribute("src", "/v1/session/qr/image?c=code1");
    expect(screen.getByText("Scan with your phone to sign in")).toBeInTheDocument();
    expect(screen.getByLabelText("Password")).toBeInTheDocument();
    expect(screen.getByText("Or type 482 911 there instead of scanning.")).toBeInTheDocument();
    expect(screen.getByText(/Account → Scan QR code/)).toBeInTheDocument();
  });

  it("polls every 2 s and opens the app once the phone approves", async () => {
    let answer: Poll = "pending";
    const s = server({ poll: () => answer });
    render(() => <QrLogin />);
    await image();
    await run(POLL_MS);
    expect(s.urls()).toContain("GET /v1/session/qr/poll?t=tok1");
    expect(session().status).toBe("anon");

    answer = "approved";
    await run(POLL_MS);
    await until(() => expect(session()).toEqual({ status: "authed", deviceName: "web" }));
    expect(screen.getByText("Approved. Signing you in…")).toBeInTheDocument();
    const polls = s.urls().filter((u) => u.includes("/poll")).length;
    await run(POLL_MS * 3);
    expect(s.urls().filter((u) => u.includes("/poll")).length).toBe(polls);
  });

  it("gets a new code when the old one expires", async () => {
    const s = server({ expiresIn: 3 });
    render(() => <QrLogin />);
    await image();
    await run(4_000);
    await until(() => expect(img()).toHaveAttribute("src", "/v1/session/qr/image?c=code2"));
    expect(s.urls().filter((u) => u === "POST /v1/session/qr")).toHaveLength(2);
  });

  it("asks for a new code when the server says the old one expired", async () => {
    let answer: Poll = "expired";
    server({ poll: () => answer });
    render(() => <QrLogin />);
    await image();
    vi.advanceTimersByTime(POLL_MS);
    answer = "pending";
    await until(() => expect(img()).toHaveAttribute("src", "/v1/session/qr/image?c=code2"));
  });

  it("stops after a few codes until asked again", async () => {
    const s = server({ expiresIn: 1 });
    render(() => <QrLogin />);
    await image();
    for (let i = 0; i < MAX_CODES + 2; i++) {
      vi.advanceTimersByTime(POLL_MS);
      await flush();
    }
    await until(() => screen.getByRole("button", { name: "New code" }));
    const button = screen.getByRole("button", { name: "New code" });
    expect(screen.getByText("The code expired.")).toBeInTheDocument();
    expect(s.urls().filter((u) => u === "POST /v1/session/qr")).toHaveLength(MAX_CODES);
    fireEvent.click(button);
    await until(() => expect(s.urls().filter((u) => u === "POST /v1/session/qr")).toHaveLength(MAX_CODES + 1));
    expect(await image()).toBeInTheDocument();
  });

  it("says so when the phone denies, and offers a new code", async () => {
    server({ poll: () => "denied" });
    render(() => <QrLogin />);
    await image();
    vi.advanceTimersByTime(POLL_MS);
    await until(() => screen.getByText("Sign-in was declined on the other device."));
    fireEvent.click(screen.getByRole("button", { name: "New code" }));
    expect(await image()).toHaveAttribute("src", "/v1/session/qr/image?c=code2");
    expect(session().status).toBe("anon");
  });

  it("does not poll while the page is hidden", async () => {
    const s = server();
    render(() => <QrLogin />);
    await image();
    const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    vi.advanceTimersByTime(POLL_MS * 3);
    expect(s.urls().some((u) => u.includes("/poll"))).toBe(false);
    visibility.mockReturnValue("visible");
    vi.advanceTimersByTime(POLL_MS);
    await until(() => expect(s.urls().some((u) => u.includes("/poll"))).toBe(true));
  });

  it("waits out a rate limit before asking again", async () => {
    let limited = true;
    const calls = stubFetch((c) => {
      if (c.url === "/v1/session/qr" && limited) {
        return { status: 429, body: { error: { code: "rate_limited", message: "slow" } }, headers: { "Retry-After": "5" } };
      }
      if (c.url === "/v1/session/qr") return { body: { code: "late", pollToken: "t", approveUrl: "u", expiresIn: 120 } };
      return { body: { status: "pending" } };
    });
    render(() => <QrLogin />);
    await run(0);
    expect(calls).toHaveLength(1);
    expect(screen.getByRole("status")).toHaveTextContent("Getting a code…");
    limited = false;
    await run(4_000);
    expect(calls).toHaveLength(1);
    await run(2_000);
    expect(await image()).toHaveAttribute("src", "/v1/session/qr/image?c=late");
  });

  it("is not shown on /approve (the phone signing in to approve)", () => {
    window.history.replaceState(null, "", "/approve?c=abc");
    try {
      const s = server();
      render(() => <Login />);
      expect(screen.queryByText("Scan with your phone to sign in")).toBeNull();
      expect(s.calls).toHaveLength(0);
      // The phone's browser is not signed in: point to the app's scanner.
      expect(screen.getByRole("note")).toHaveTextContent("Approve from the Secretary app");
      expect(screen.getByRole("note")).toHaveTextContent("Account → Scan QR code");
      expect(screen.getByLabelText("Password")).toBeInTheDocument();
    } finally {
      window.history.replaceState(null, "", "/");
    }
  });
});
