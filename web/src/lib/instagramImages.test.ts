import { createRoot, createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { IgItem } from "../api/instagram";
import { createFreshSrc, freshUrl, isExpired, keepSigned, signedExpiry } from "./instagramImages";

const NOW = 1_800_000_000;
const link = (exp: number, name = "abc") => `/v1/instagram/s/${exp}/deadbeef/${name}`;

function stubResign(answer: (urls: string[]) => Record<string, string>) {
  const calls: string[][] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      expect(url).toBe("/v1/instagram/resign");
      const urls = (JSON.parse(String(init?.body)) as { urls: string[] }).urls;
      calls.push(urls);
      return new Response(JSON.stringify({ ok: true, data: { urls: answer(urls) } }), { status: 200, headers: { "content-type": "application/json" } });
    }),
  );
  return calls;
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(NOW * 1000);
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("signed links", () => {
  it("reads the expiry of media and archive links only", () => {
    expect(signedExpiry(link(NOW))).toBe(NOW);
    expect(signedExpiry(`/v1/instagram/a/${NOW}/deadbeef/itm_1`)).toBe(NOW);
    expect(signedExpiry("/v1/instagram/s/9/x/fixture")).toBeNull();
    expect(signedExpiry("/v1/media/items/1/thumb")).toBeNull();
  });

  it("counts a link about to expire as expired", () => {
    expect(isExpired(link(NOW - 1), NOW)).toBe(true);
    expect(isExpired(link(NOW + 60), NOW)).toBe(true);
    expect(isExpired(link(NOW + 3600), NOW)).toBe(false);
    expect(isExpired("/v1/instagram/s/9/x/fixture", NOW)).toBe(false);
  });

  it("re-signs everything that asks at once in one request, and keeps a link it gets no answer for", async () => {
    const calls = stubResign(() => ({ [link(NOW - 5, "a")]: link(NOW + 3600, "a") }));
    const a = freshUrl(link(NOW - 5, "a"));
    const b = freshUrl(link(NOW - 5, "b"));
    await vi.advanceTimersByTimeAsync(30);
    expect(await a).toBe(link(NOW + 3600, "a"));
    expect(await b).toBe(link(NOW - 5, "b"));
    expect(calls).toEqual([[link(NOW - 5, "a"), link(NOW - 5, "b")]]);
    // A valid link asks nobody, unless it is known to have failed.
    expect(await freshUrl(link(NOW + 3600))).toBe(link(NOW + 3600));
    expect(calls).toHaveLength(1);
  });
});

describe("createFreshSrc", () => {
  it("never shows an expired link: nothing, then the fresh one", async () => {
    const calls = stubResign(() => ({ [link(NOW - 5)]: link(NOW + 3600) }));
    await createRoot(async (dispose) => {
      const fresh = createFreshSrc(() => link(NOW - 5));
      expect(fresh.src()).toBe("");
      await vi.advanceTimersByTimeAsync(30);
      expect(fresh.src()).toBe(link(NOW + 3600));
      expect(calls).toHaveLength(1);
      dispose();
    });
  });

  it("asks again after a failure, says so after a few tries, keeps asking, and a load clears it", async () => {
    stubResign((urls) => Object.fromEntries(urls.map((u) => [u, link(NOW + 3600)])));
    const giveUp = vi.fn();
    await createRoot(async (dispose) => {
      const fresh = createFreshSrc(() => link(NOW + 3600), giveUp);
      expect(fresh.src()).toBe(link(NOW + 3600));
      await vi.advanceTimersByTimeAsync(0);
      fresh.onError();
      expect(fresh.retrying()).toBe(true);
      expect(fresh.failed()).toBe(false);
      await vi.advanceTimersByTimeAsync(30);
      expect(fresh.src()).toBe(`${link(NOW + 3600)}?r=1`);
      fresh.onError();
      await vi.advanceTimersByTimeAsync(3_100);
      expect(fresh.src()).toBe(`${link(NOW + 3600)}?r=2`);
      fresh.onError();
      expect(fresh.failed()).toBe(true);
      expect(giveUp).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(15_100);
      expect(fresh.src()).toBe(`${link(NOW + 3600)}?r=3`);
      fresh.onError();
      expect(giveUp).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(60_100);
      expect(fresh.src()).toBe(`${link(NOW + 3600)}?r=4`);
      fresh.onLoad();
      expect(fresh.failed()).toBe(false);
      expect(fresh.retrying()).toBe(false);
      dispose();
    });
  });

  it("starts over when the page brings a new link", async () => {
    stubResign(() => ({}));
    await createRoot(async (dispose) => {
      const [url, setUrl] = createSignal(link(NOW + 3600, "old"));
      const fresh = createFreshSrc(url);
      await vi.advanceTimersByTimeAsync(0);
      for (let i = 0; i < 3; i += 1) fresh.onError();
      expect(fresh.failed()).toBe(true);
      setUrl(link(NOW + 3600, "new"));
      await vi.advanceTimersByTimeAsync(0);
      expect(fresh.failed()).toBe(false);
      expect(fresh.src()).toBe(link(NOW + 3600, "new"));
      dispose();
    });
  });
});

describe("keepSigned", () => {
  const item = (exp: number, caption = "c"): IgItem => ({ id: "a", kind: "image", url: link(exp), thumbUrl: link(exp, "t"), caption });

  it("keeps the object when only good signatures changed, so its tile does not reload", () => {
    const old = item(NOW + 3600);
    expect(keepSigned([old], [item(NOW + 4500)])[0]).toBe(old);
  });

  it("takes the new one when anything else changed, or the old link is about to expire", () => {
    const changed = item(NOW + 4500, "new caption");
    expect(keepSigned([item(NOW + 3600)], [changed])[0]).toBe(changed);
    const fresh = item(NOW + 4500);
    expect(keepSigned([item(NOW + 300)], [fresh])[0]).toBe(fresh);
  });
});
