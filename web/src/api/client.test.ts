import { describe, expect, it } from "vitest";
import { stubFetch } from "../test/fetch";
import { ApiError, request } from "./client";

describe("request", () => {
  it("sends the CSRF header and same-origin credentials on writes only", async () => {
    const calls = stubFetch(() => ({ body: { ok: true } }));
    await request("GET", "/v1/health");
    await request("POST", "/v1/shopping", { body: { rawName: "x" } });
    await request("PATCH", "/v1/shopping/1", { body: { checked: true } });
    await request("DELETE", "/v1/session");
    expect(calls.map((c) => c.headers["x-secretary-csrf"])).toEqual([undefined, "1", "1", "1"]);
    expect(calls.every((c) => c.credentials === "same-origin")).toBe(true);
    expect(calls[1]!.body).toEqual({ rawName: "x" });
  });

  it("parses the error envelope and Retry-After", async () => {
    stubFetch(() => ({
      status: 429,
      body: { error: { code: "rate_limited", message: "Slow down" } },
      headers: { "Retry-After": "12" },
    }));
    const err = await request("POST", "/v1/session", { body: { token: "x" } }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(err).toMatchObject({ status: 429, code: "rate_limited", message: "Slow down", retryAfterSeconds: 12 });
  });

  it("flags Lucina outages", async () => {
    stubFetch(() => ({ status: 502, body: { error: { code: "lucina_unreachable", message: "down" } } }));
    const err = (await request("POST", "/v1/chat", { body: {} }).catch((e: unknown) => e)) as ApiError;
    expect(err.lucinaOffline).toBe(true);
  });

  it("maps network failures to status 0", async () => {
    stubFetch(() => {
      throw new TypeError("Failed to fetch");
    });
    const err = (await request("GET", "/v1/health").catch((e: unknown) => e)) as ApiError;
    expect(err.status).toBe(0);
    expect(err.code).toBe("network");
  });
});
