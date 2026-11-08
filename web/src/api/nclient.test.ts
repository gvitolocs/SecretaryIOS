import { afterEach, describe, expect, it, vi } from "vitest";
import { deferred, stubFetch } from "../test/fetch";
import { nclientApi, nclientImageUrl, saveFinished } from "./nclient";

describe("NClient API", () => {
  afterEach(() => vi.restoreAllMocks());

  it("uses the shared authenticated client and unwraps the server envelope", async () => {
    const calls = stubFetch(() => ({ body: { ok: true, data: { enabled: true } } }));
    expect(await nclientApi.status()).toEqual({ enabled: true });
    await nclientApi.search({ q: "a & b", page: 2, sort: "popular-week", tags: [1, 2], exclude: [3] });
    const query = new URL(calls[1]!.url, "http://localhost").searchParams;
    expect(query.get("q")).toBe("a & b");
    expect(query.get("tags")).toBe("1,2");
    expect(query.get("exclude")).toBe("3");
    expect(query.get("sort")).toBe("popular-week");
    await nclientApi.favorite(7, true);
    await nclientApi.favorite(7, false);
    await nclientApi.saveProgress(7, 2, 10);
    for (const call of calls) expect(call.credentials).toBe("same-origin");
    for (const call of calls.filter(c => c.method !== "GET")) expect(call.headers["x-secretary-csrf"]).toBe("1");
    expect(calls[4]!.body).toEqual({ page: 2, pages: 10 });
    expect(calls.map(c => c.method)).toEqual(["GET", "GET", "PUT", "DELETE", "PUT"]);
  });

  it("serializes progress across reading sessions and continues after a failed save", async () => {
    const first = deferred<{ status: number }>();
    const calls = stubFetch(() => calls.length === 1 ? first.promise : { body: { ok: true, data: {} } });
    const older = nclientApi.saveProgress(7, 1, 10);
    const failed = expect(older).rejects.toHaveProperty("status", 500);
    const newer = nclientApi.saveProgress(7, 5, 10);
    await vi.waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0]!.body).toEqual({ page: 1, pages: 10 });
    first.resolve({ status: 500 });
    await failed;
    await newer;
    expect(calls.map(c => c.body)).toEqual([{ page: 1, pages: 10 }, { page: 5, pages: 10 }]);
  });

  it("starts, polls and cancels a save to the Gallery", async () => {
    const job = { job: "ab12", state: "running", total: 4, done: 1, saved: [], failed: [] };
    const calls = stubFetch(() => ({ status: 202, body: { ok: true, data: job } }));
    expect(await nclientApi.save(7)).toEqual(job);
    expect(calls[0]).toMatchObject({ method: "POST", url: "/v1/nclient/galleries/7/save", body: { target: "gallery" } });
    expect(calls[0]!.headers["x-secretary-csrf"]).toBe("1");
    await nclientApi.saveStatus("ab12");
    await nclientApi.cancelSave("ab12");
    expect(calls.slice(1).map(c => `${c.method} ${c.url}`)).toEqual(["GET /v1/nclient/saves/ab12", "DELETE /v1/nclient/saves/ab12"]);
    expect(["done", "failed", "cancelled"].every(s => saveFinished(s as never))).toBe(true);
    expect(saveFinished("running")).toBe(false);
    expect(saveFinished("queued")).toBe(false);
  });

  it("accepts only signed proxy URLs returned by the server", () => {
    expect(nclientImageUrl("/v1/nclient/s/7/page/0/123/abc_DEF-123")).toBeDefined();
    for (const url of ["https://example.org/image.jpg", "//example.org/image.jpg", "/v1/media/image", "/v1/nclient/s/7/page/0/123/sig?redirect=x", "data:image/png;base64,x"]) {
      expect(nclientImageUrl(url)).toBeUndefined();
    }
  });
});
