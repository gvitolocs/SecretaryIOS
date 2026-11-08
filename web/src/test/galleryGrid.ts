import { vi } from "vitest";

/** jsdom has no layout: give the Gallery's scroll box a size so the grid draws its rows. */
export function sizeTheBox(width = 1000, height = 800): void {
  vi.spyOn(Element.prototype, "clientWidth", "get").mockImplementation(function (this: Element) {
    return this.classList.contains("g-scroll") ? width : 0;
  });
  vi.spyOn(Element.prototype, "clientHeight", "get").mockImplementation(function (this: Element) {
    return this.classList.contains("g-scroll") ? height : 0;
  });
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
}

/** A synthetic item as the server sends it (day 28 of October 2026, `n` days back). */
export function rawItem(id: string, extra: Record<string, unknown> = {}, daysBack = 0): Record<string, unknown> {
  const day = new Date(2026, 9, 28 - daysBack, 12).toISOString();
  return {
    id,
    kind: "photo",
    title: `${id}.jpg`,
    mime: "image/jpeg",
    size: 1000,
    taken_at: day,
    created_at: day,
    width: 4000,
    height: 3000,
    source: "ios-photos",
    device: "iphone-test",
    device_name: "Test iPhone",
    origin: { filename: null, folder: null, imported_from: null },
    favorite: false,
    rotation: 0,
    albums: [],
    thumbhash: null,
    urls: { thumb: `/v1/media/items/${id}/thumb?size=256`, download: `/v1/media/items/${id}/original?download=1` },
    ...extra,
  };
}
