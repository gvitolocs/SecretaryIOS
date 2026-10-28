import type { Exif, GalleryItem, ItemDetails } from "../api/gallery";

/** A synthetic Gallery item (no real names or files). */
export function galleryItem(id: string, over: Partial<GalleryItem> = {}): GalleryItem {
  return {
    id,
    kind: "photo",
    title: `${id}.jpg`,
    caption: null,
    mime: "image/jpeg",
    size: 2_400_000,
    takenAt: "2026-03-04T12:03:00Z",
    addedAt: "2026-03-05T09:00:00Z",
    width: 4000,
    height: 3000,
    durationMs: null,
    place: null,
    camera: null,
    source: "ios-library",
    device: "dev-1",
    deviceName: "Test iPhone",
    origin: { filename: `IMG_${id}.JPG`, folder: "DCIM/100TEST", importedFrom: null },
    favorite: false,
    rotation: 0,
    edited: false,
    albums: [],
    trashedAt: null,
    trashExpiresAt: null,
    thumbhash: null,
    urls: {
      thumb: `/v1/media/items/${id}/r/thumb`,
      large: `/v1/media/items/${id}/r/large`,
      original: `/v1/media/items/${id}/original`,
      download: `/v1/media/items/${id}/download`,
    },
    ...over,
  };
}

export function exif(over: Partial<Exif> = {}): Exif {
  return {
    make: "Apple",
    model: "Apple iPhone 15",
    lens: "Main camera 6.9mm",
    iso: 1600,
    exposureS: 0.004,
    fNumber: 1.8,
    focalMm: 6.9,
    flash: false,
    orientation: 1,
    software: null,
    takenAt: null,
    offset: "+02:00",
    ...over,
  };
}

export function details(item: GalleryItem, over: Partial<ItemDetails> = {}): ItemDetails {
  return { item, exif: exif(), people: [], ...over };
}
