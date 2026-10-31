import { beforeEach, describe, expect, it, vi } from "vitest";
import { createAlbumShare, createSelectionShare, getRenditions, listShares, parseRenditions, parseShare, revokeShare } from "./gallery";

vi.mock("./client", () => ({ request: vi.fn() }));
import { request } from "./client";
const mocked = vi.mocked(request);

const row = {
  token: "AAAAAAAAAAAAAAAAAAAAAA",
  url_path: "/v1/media/a/AAAAAAAAAAAAAAAAAAAAAA",
  public_path: "/a/AAAAAAAAAAAAAAAAAAAAAA",
  kind: "selection",
  album_id: null,
  title: "Weekend",
  count: 3,
  cover_item: "itm_1",
  cover_thumb: "/v1/media/s/t/itm_1/thumb.jpg",
  created_at: "2026-10-28T08:00:00.000Z",
  expires_at: "2026-11-04T08:00:00.000Z",
  revoked_at: null,
  allow_original: true,
  views: 4,
  last_view_at: "2026-10-28T09:00:00.000Z",
  live: true,
};

beforeEach(() => mocked.mockReset());

describe("links", () => {
  it("reads a link row, dropping what isn't a same-origin path", () => {
    expect(parseShare(row)).toEqual({
      token: row.token,
      kind: "selection",
      albumId: null,
      title: "Weekend",
      count: 3,
      coverItem: "itm_1",
      coverThumb: "/v1/media/s/t/itm_1/thumb.jpg",
      createdAt: row.created_at,
      expiresAt: row.expires_at,
      revokedAt: null,
      allowOriginal: true,
      views: 4,
      lastViewAt: row.last_view_at,
      live: true,
      publicPath: `/a/${row.token}`,
    });
    expect(parseShare({ ...row, cover_thumb: "https://elsewhere.example/x.jpg" })?.coverThumb).toBeNull();
    expect(parseShare({ ...row, kind: "album", album_id: "alb_1" })).toMatchObject({ kind: "album", albumId: "alb_1" });
    expect(parseShare({})).toBeNull();
    expect(parseShare(null)).toBeNull();
    // A server without public_path still gets a working path.
    expect(parseShare({ token: "T" })?.publicPath).toBe("/a/T");
  });

  it("creates a link over chosen photos: the ids, the expiry in days, the right to download, a name", async () => {
    mocked.mockResolvedValue(row);
    const share = await createSelectionShare(["a", "b"], { expiresInDays: 7, allowOriginal: true, title: "  Weekend " });
    expect(share.token).toBe(row.token);
    expect(mocked).toHaveBeenCalledWith("POST", "/v1/media/shares", { body: { item_ids: ["a", "b"], allow_original: true, expires_in_days: 7, title: "Weekend" } });
  });

  it("sends no expiry for a link that never expires and no empty name", async () => {
    mocked.mockResolvedValue(row);
    await createSelectionShare(["a"], { expiresInDays: null, allowOriginal: false, title: "   " });
    expect(mocked).toHaveBeenCalledWith("POST", "/v1/media/shares", { body: { item_ids: ["a"], allow_original: false } });
  });

  it("creates an album link on the album's own route", async () => {
    mocked.mockResolvedValue({ ...row, kind: "album", album_id: "alb 1" });
    await createAlbumShare("alb 1", { expiresInDays: 30, allowOriginal: false });
    expect(mocked).toHaveBeenCalledWith("POST", "/v1/media/albums/alb%201/shares", { body: { allow_original: false, expires_in_days: 30 } });
  });

  it("fails when the answer isn't a link", async () => {
    mocked.mockResolvedValue({});
    await expect(createSelectionShare(["a"], { expiresInDays: 1, allowOriginal: false })).rejects.toThrow();
  });

  it("lists both kinds and revokes by token", async () => {
    mocked.mockResolvedValueOnce({ shares: [row, { ...row, token: "BBBBBBBBBBBBBBBBBBBBBB", kind: "album", album_id: "alb_1" }, { nope: 1 }] });
    expect((await listShares()).map((s) => s.kind)).toEqual(["selection", "album"]);
    expect(mocked).toHaveBeenLastCalledWith("GET", "/v1/media/shares");
    mocked.mockResolvedValueOnce(undefined);
    await revokeShare("A/B");
    expect(mocked).toHaveBeenLastCalledWith("DELETE", "/v1/media/shares/A%2FB");
  });
});

describe("renditions", () => {
  const body = {
    item_id: "itm_1",
    kind: "photo",
    original: { version: 2, format: "heic", mime: "image/heic", bytes: 2_000_000, blake3: "ab", width: 4032, height: 3024, stored: "store", reachable: true, url: "/v1/media/items/itm_1/original" },
    versions: [{ version: 1, format: "heic", mime: "image/heic", bytes: 1_900_000, blake3: "cd", replaced_at: "2026-10-27T00:00:00.000Z", stored: "store", reachable: false, url: "/v1/media/items/itm_1/original?version=1" }],
    renditions: [
      { name: "preview", format: "jxl", mime: "image/jxl", bytes: 57_000, width: 1440, height: 1080, url: "/v1/media/items/itm_1/r/preview?v=2" },
      { name: "mystery", format: "jxl", mime: "image/jxl", bytes: 1, url: "/x" },
      { name: "thumb", format: "jpeg", mime: "image/jpeg", bytes: 1, url: "https://elsewhere.example/t" },
    ],
    device: {
      id: "dev-1",
      name: "Test iPhone",
      kind: "iphone",
      last_seen_at: "2026-10-27T08:00:00.000Z",
      asset_id: "A1",
      uploads: [
        { filename: "IMG_1.HEIC", mime: "image/heic", uti: "public.heic", bytes: 1_900_000, blake3: "cd", width: 4032, height: 3024, uploaded_at: "2026-10-26T00:00:00.000Z", matches_version: 1 },
        { filename: "IMG_1.HEIC", mime: "image/heic", bytes: 2_000_000, blake3: "ab", uploaded_at: "2026-10-27T00:00:00.000Z", matches_version: 2 },
      ],
    },
  };

  it("reads the original, versions, known renditions and the device's uploads", () => {
    const r = parseRenditions(body)!;
    expect(r.original).toMatchObject({ format: "heic", version: 2, stored: "store", reachable: true, bytes: 2_000_000 });
    expect(r.versions).toHaveLength(1);
    expect(r.versions[0]).toMatchObject({ version: 1, reachable: false, replacedAt: "2026-10-27T00:00:00.000Z" });
    // An unknown rendition name and a URL off this origin are dropped.
    expect(r.renditions.map((x) => x.name)).toEqual(["preview"]);
    expect(r.device?.uploads).toHaveLength(2);
    expect(r.device?.latest).toMatchObject({ bytes: 2_000_000, blake3: "ab", matchesVersion: 2, uti: null });
    expect(r.device).toMatchObject({ id: "dev-1", name: "Test iPhone", assetId: "A1" });
  });

  it("has no device block for a picture that came from nowhere, and is null without an original", () => {
    expect(parseRenditions({ ...body, device: null })?.device).toBeNull();
    expect(parseRenditions({ item_id: "x" })).toBeNull();
    expect(parseRenditions(null)).toBeNull();
  });

  it("asks for an item's renditions", async () => {
    mocked.mockResolvedValue(body);
    expect((await getRenditions("itm/1"))?.itemId).toBe("itm_1");
    expect(mocked).toHaveBeenCalledWith("GET", "/v1/media/items/itm%2F1/renditions");
  });
});
