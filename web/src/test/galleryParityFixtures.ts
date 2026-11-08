import type { GalleryShare, ItemRenditions } from "../api/gallery";

/** A synthetic link row (no real names or tokens). */
export function galleryShare(token: string, over: Partial<GalleryShare> = {}): GalleryShare {
  return {
    token,
    kind: "selection",
    albumId: null,
    title: "Shared photos",
    count: 2,
    coverItem: "a",
    coverThumb: `/v1/media/s/t/${token}/thumb.jpg`,
    createdAt: "2026-10-20T10:00:00.000Z",
    expiresAt: null,
    revokedAt: null,
    allowOriginal: false,
    views: 0,
    lastViewAt: null,
    live: true,
    publicPath: `/a/${token}`,
    ...over,
  };
}

const HASH = "a".repeat(64);

/** Every stored form of a synthetic JPEG from a device (original, a preview, a large JPEG, the grid tile). */
export function itemRenditions(over: Partial<ItemRenditions> = {}): ItemRenditions {
  return {
    itemId: "a",
    kind: "photo",
    original: {
      format: "jpeg",
      mime: "image/jpeg",
      bytes: 2_400_000,
      blake3: HASH,
      width: 4000,
      height: 3000,
      url: "/v1/media/items/a/original",
      version: 1,
      stored: "store",
      reachable: true,
    },
    versions: [],
    renditions: [
      { name: "grid", format: "jxl", mime: "image/jxl", bytes: 8_800, blake3: null, width: 256, height: 256, url: "/v1/media/items/a/r/grid?v=1" },
      { name: "preview", format: "jxl", mime: "image/jxl", bytes: 57_000, blake3: null, width: 1440, height: 1080, url: "/v1/media/items/a/r/preview?v=1" },
      { name: "thumb", format: "jpeg", mime: "image/jpeg", bytes: 38_000, blake3: null, width: 480, height: 360, url: "/v1/media/items/a/thumb?size=256&v=1" },
      { name: "large", format: "jpeg", mime: "image/jpeg", bytes: 135_000, blake3: null, width: 1600, height: 1200, url: "/v1/media/items/a/thumb?size=1024&v=1" },
    ],
    device: {
      id: "dev-1",
      name: "Test iPhone",
      kind: "iphone",
      lastSeenAt: "2026-10-27T08:00:00.000Z",
      assetId: "ASSET-1",
      uploads: [
        {
          filename: "IMG_0001.JPG",
          mime: "image/jpeg",
          uti: "public.jpeg",
          bytes: 2_400_000,
          blake3: HASH,
          width: 4000,
          height: 3000,
          takenAt: null,
          uploadedAt: "2026-10-27T07:59:00.000Z",
          matchesVersion: 1,
        },
      ],
      latest: null,
    },
    ...over,
  };
}

/** `itemRenditions` with `device.latest` filled in from its last upload (as the parser does). */
export function withLatest(r: ItemRenditions): ItemRenditions {
  if (!r.device) return r;
  return { ...r, device: { ...r.device, latest: r.device.uploads[r.device.uploads.length - 1] ?? null } };
}
