import { afterEach, describe, expect, it, vi } from "vitest";
import { uploadToLucina, type LucinaDevice } from "./mediaUpload";

const MAC: LucinaDevice = { id: "mac-0b1c", name: "MacBook Air", kind: "mac" };

interface Seen {
  method: string;
  url: string;
  headers: Record<string, string>;
  json?: Record<string, unknown>;
  bytes?: number;
}

/** A fake lucina-media upload endpoint; `failPut` makes that many chunk PUTs fail. */
function fakeServer(opts: { duplicate?: boolean; failPut?: number } = {}) {
  const seen: Seen[] = [];
  let received = 0;
  let failures = opts.failPut ?? 0;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init: RequestInit = {}) => {
      const url = String(input);
      const method = init.method ?? "GET";
      const headers: Record<string, string> = {};
      for (const [k, v] of Object.entries((init.headers ?? {}) as Record<string, string>)) headers[k.toLowerCase()] = v;
      const s: Seen = { method, url, headers };
      if (typeof init.body === "string") s.json = JSON.parse(init.body) as Record<string, unknown>;
      if (init.body instanceof Blob) s.bytes = init.body.size;
      seen.push(s);
      const json = (body: unknown) => new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
      const state = () => ({ upload_id: "upl-1", offset: received, size: 10, finished: false, duplicate: false, item_id: null });
      if (method === "PUT" && url.startsWith("/v1/media/devices/")) return json({ id: MAC.id, name: MAC.name, kind: "mac" });
      if (method === "POST" && url === "/v1/media/uploads") {
        return json(opts.duplicate ? { upload_id: null, offset: 10, size: 10, finished: true, duplicate: true, item_id: "itm-old" } : state());
      }
      if (method === "PUT" && url.startsWith("/v1/media/uploads/upl-1?offset=")) {
        if (failures > 0) {
          failures--;
          // The bytes arrived but the answer was lost.
          received += s.bytes ?? 0;
          throw new TypeError("network down");
        }
        expect(Number(new URL(url, "http://x").searchParams.get("offset"))).toBe(received);
        received += s.bytes ?? 0;
        return json(state());
      }
      if (method === "GET" && url === "/v1/media/uploads/upl-1") return json(state());
      if (method === "POST" && url === "/v1/media/uploads/upl-1/finish") return json({ item_id: "itm-new", duplicate: false, version: 1 });
      return new Response("{}", { status: 404 });
    }),
  );
  return seen;
}

afterEach(() => vi.unstubAllGlobals());

describe("uploadToLucina", () => {
  it("registers the Mac, uploads in chunks with the device tag and finishes", async () => {
    const seen = fakeServer();
    const progress: number[] = [];
    const file = new Blob(["0123456789"], { type: "image/jpeg" });
    const result = await uploadToLucina(file, {
      name: "IMG_0001.jpg",
      device: MAC,
      assetId: "path-hash-1",
      takenAt: "2026-10-27T10:00:00Z",
      chunkBytes: 4,
      onProgress: (sent) => progress.push(sent),
    });
    expect(result).toEqual({ itemId: "itm-new", duplicate: false });
    expect(seen[0]).toMatchObject({ method: "PUT", url: "/v1/media/devices/mac-0b1c", json: { name: "MacBook Air", kind: "mac" } });
    expect(seen[1]!.json).toEqual({
      filename: "IMG_0001.jpg",
      size: 10,
      meta: { device: "mac-0b1c", asset_id: "path-hash-1", source: "mac", filename: "IMG_0001.jpg", mime: "image/jpeg", taken_at: "2026-10-27T10:00:00Z" },
    });
    const puts = seen.filter((s) => s.method === "PUT" && s.url.includes("/uploads/"));
    expect(puts.map((p) => p.bytes)).toEqual([4, 4, 2]);
    expect(puts.every((p) => p.headers["x-secretary-csrf"] === "1")).toBe(true);
    expect(progress).toEqual([0, 4, 8, 10]);
    expect(seen.at(-1)!.url).toBe("/v1/media/uploads/upl-1/finish");
  });

  it("stops early when the server already has the picture", async () => {
    const seen = fakeServer({ duplicate: true });
    const result = await uploadToLucina(new Blob(["0123456789"]), { name: "a.png", device: MAC });
    expect(result).toEqual({ itemId: "itm-old", duplicate: true });
    expect(seen.some((s) => s.url.includes("?offset="))).toBe(false);
    // Without an asset id, the content's SHA-256 names the asset.
    expect((seen[1]!.json!.meta as { asset_id: string }).asset_id).toMatch(/^[0-9a-f]{64}$/);
  });

  it("resumes from the server's offset after a lost answer", async () => {
    const seen = fakeServer({ failPut: 1 });
    const result = await uploadToLucina(new Blob(["0123456789"]), { name: "b.jpg", device: MAC, assetId: "b", chunkBytes: 4 });
    expect(result.itemId).toBe("itm-new");
    const offsets = seen.filter((s) => s.url.includes("?offset=")).map((s) => new URL(s.url, "http://x").searchParams.get("offset"));
    expect(offsets).toEqual(["0", "4", "8"]);
    expect(seen.some((s) => s.method === "GET" && s.url === "/v1/media/uploads/upl-1")).toBe(true);
  });

  it("refuses a bad device id or file name before sending anything", async () => {
    const seen = fakeServer();
    await expect(uploadToLucina(new Blob(["x"]), { name: "a.jpg", device: { ...MAC, id: "../etc" } })).rejects.toThrow();
    await expect(uploadToLucina(new Blob(["x"]), { name: "a/b.jpg", device: MAC })).rejects.toThrow();
    expect(seen).toEqual([]);
  });
});
