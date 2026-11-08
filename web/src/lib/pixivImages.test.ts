import { createRoot, createSignal } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";
import { stubFetch } from "../test/fetch";
import { createFreshSrc, freshUrl, isExpired, signedExpiry } from "./pixivImages";

const NOW = 1_800_000_000;
const tok = (s: string) => Array.from(new TextEncoder().encode(s), (b) => b.toString(16).padStart(2, "0")).join("");
const A = tok("https://i.pximg.net/a_p0_master1200.jpg");
const B = tok("https://i.pximg.net/b_p0_master1200.jpg");
const link = (exp: number, token: string) => `/v1/pixiv/s/${exp}/${token}/abcdef0123`;

function serveResign(map: (token: string) => string) {
  return stubFetch((call) => {
    if (call.url === "/v1/pixiv/resign") {
      const tokens = (call.body as { tokens: string[] }).tokens;
      return { body: { ok: true, data: { urls: Object.fromEntries(tokens.map((t) => [t, map(t)])) } } };
    }
    return { status: 404, body: {} };
  });
}

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("signed links", () => {
  it("reads the expiry of real signed links only", () => {
    expect(signedExpiry(link(NOW, A))).toBe(NOW);
    expect(signedExpiry("/v1/pixiv/s/1/11/p0")).toBeNull(); // not a real timestamp (test fixtures)
    expect(signedExpiry("https://example.com/x.jpg")).toBeNull();
    expect(isExpired(link(NOW, A), NOW + 1)).toBe(true);
    expect(isExpired(link(NOW, A), NOW - 60)).toBe(true); // within the 2-minute margin
    expect(isExpired(link(NOW, A), NOW - 3600)).toBe(false);
    expect(isExpired("/v1/pixiv/s/1/11/p0", NOW)).toBe(false);
  });

  it("re-signs expired links in one request and keeps valid ones without asking", async () => {
    const calls = serveResign((t) => (t === A ? link(NOW + 30 * 86400, A) : ""));
    const valid = link(Math.floor(Date.now() / 1000) + 86400, B);
    expect(await freshUrl(valid)).toBe(valid);
    expect(calls).toHaveLength(0);
    const oldA = link(NOW - 86400 * 400, A);
    const oldB = link(NOW - 86400 * 400, B);
    const [a, b] = await Promise.all([freshUrl(oldA), freshUrl(oldB)]);
    expect(a).toBe(link(NOW + 30 * 86400, A));
    expect(b).toBe(oldB); // refused: the caller keeps what it had
    expect(calls).toHaveLength(1);
    expect((calls[0]!.body as { tokens: string[] }).tokens.sort()).toEqual([A, B].sort());
    // force re-signs a valid link too (an image that failed to load)
    await freshUrl(valid, true);
    expect(calls).toHaveLength(2);
  });

  it("keeps the old link when the request fails", async () => {
    stubFetch(() => ({ status: 502, body: { ok: false, error: "upstream" } }));
    const old = link(NOW - 86400 * 400, A);
    expect(await freshUrl(old)).toBe(old);
  });
});

describe("createFreshSrc", () => {
  it("shows nothing for an expired link until it is re-signed, and retries a failing image once", async () => {
    const fresh = link(Math.floor(Date.now() / 1000) + 30 * 86400, A);
    const calls = serveResign(() => fresh);
    await createRoot(async (dispose) => {
      const [raw] = createSignal(link(NOW - 86400 * 400, A));
      const s = createFreshSrc(raw);
      expect(s.src()).toBe(""); // no broken-image icon meanwhile
      await vi.waitFor(() => expect(s.src()).toBe(fresh));
      expect(calls).toHaveLength(1);
      s.onError();
      s.onError();
      await new Promise((r) => setTimeout(r, 60));
      expect(calls).toHaveLength(2); // once only
      dispose();
    });
  });

  it("shows a valid link at once", () => {
    const calls = serveResign(() => "");
    createRoot((dispose) => {
      const valid = link(Math.floor(Date.now() / 1000) + 86400, A);
      expect(createFreshSrc(() => valid).src()).toBe(valid);
      dispose();
    });
    expect(calls).toHaveLength(0);
  });
});
