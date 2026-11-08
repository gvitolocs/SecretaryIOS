import { afterEach, describe, expect, it, vi } from "vitest";
import type { GalleryItem } from "../api/gallery";
import { deferred } from "../test/fetch";
import { copyFromUrl, copyPicture, pictureFile, pictureUrl } from "./copyImage";

const photo = (over: Partial<GalleryItem> = {}): GalleryItem =>
  ({
    id: "itm1", kind: "photo", title: "Cat", mime: "image/jpeg", size: 2_000_000, edited: false,
    origin: { filename: "IMG_0001.HEIC", folder: null, importedFrom: null },
    urls: { original: "/v1/media/items/itm1/original", large: "/v1/media/items/itm1/large", preview: "/v1/media/items/itm1/preview" },
    ...over,
  }) as GalleryItem;

/** jsdom's Blob cannot back a real Response: what copyImage reads of one. */
const picture = (type: string) => ({ ok: true, status: 200, blob: async () => new Blob(["x"], { type }) }) as unknown as Response;

class FakeClipboardItem {
  constructor(public items: Record<string, Promise<Blob> | Blob>) {}
}

describe("copyImage", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    delete window.secretaryShell;
  });

  it("picks a picture the browser can decode: the original, the edited copy, else the 1024 px JPEG; never JPEG XL", () => {
    expect(pictureUrl(photo())).toBe("/v1/media/items/itm1/original");
    expect(pictureUrl(photo({ edited: true, urls: { original: "/o", edited: "/e" } }))).toBe("/e");
    expect(pictureUrl(photo({ mime: "image/heic" }))).toBe("/v1/media/items/itm1/large");
    expect(pictureUrl(photo({ size: 80_000_000 }))).toBe("/v1/media/items/itm1/large");
    expect(pictureUrl(photo({ kind: "video", urls: { poster: "/poster.jpg" } }))).toBe("/poster.jpg");
    expect(pictureUrl(photo({ kind: "video", urls: {} }))).toBeNull();
  });

  it("writes a PNG to the clipboard, starting the write before the picture has loaded (Safari needs that)", async () => {
    const loaded = deferred<Response>();
    vi.stubGlobal("fetch", vi.fn(() => loaded.promise));
    vi.stubGlobal("ClipboardItem", FakeClipboardItem);
    const write = vi.fn(async (items: FakeClipboardItem[]) => {
      await items[0]!.items["image/png"];
    });
    vi.stubGlobal("navigator", { ...navigator, clipboard: { write } });
    // A PNG needs no re-encoding, which keeps canvas out of the test.
    const copied = copyPicture(photo({ mime: "image/png" }));
    expect(write).toHaveBeenCalledOnce();
    expect(Object.keys(write.mock.calls[0]![0][0]!.items)).toEqual(["image/png"]);
    loaded.resolve(picture("image/png"));
    expect(await copied).toBe(true);
    expect(fetch).toHaveBeenCalledWith("/v1/media/items/itm1/original", { credentials: "include" });
  });

  it("falls back to the Mac app's clipboard when the page's is refused", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => picture("image/png")));
    vi.stubGlobal("ClipboardItem", FakeClipboardItem);
    vi.stubGlobal("navigator", { ...navigator, clipboard: { write: vi.fn(async () => { throw new DOMException("denied", "NotAllowedError"); }) } });
    const copyImage = vi.fn(async (_url: string) => true);
    window.secretaryShell = { showCode() {}, copyImage } as unknown as NonNullable<Window["secretaryShell"]>;
    expect(await copyPicture(photo({ mime: "image/png" }))).toBe(true);
    expect(copyImage.mock.calls[0]![0]).toMatch(/^data:image\/png;base64,/);
  });

  it("hands the Mac app the picture's address when the page cannot load it (Pixiv and Instagram pictures from elsewhere)", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => { throw new TypeError("Failed to fetch"); }));
    vi.stubGlobal("ClipboardItem", FakeClipboardItem);
    vi.stubGlobal("navigator", { ...navigator, clipboard: { write: vi.fn(async (items: FakeClipboardItem[]) => { await items[0]!.items["image/png"]; }) } });
    const copyImage = vi.fn(async (_source: string) => true);
    window.secretaryShell = { showCode() {}, copyImage } as unknown as NonNullable<Window["secretaryShell"]>;
    expect(await copyFromUrl("/v1/pixiv/img/7.jpg")).toBe(true);
    expect(copyImage).toHaveBeenCalledWith("http://localhost:3000/v1/pixiv/img/7.jpg");
  });

  it("says false when nothing can take the picture", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => picture("image/png")));
    vi.stubGlobal("navigator", { ...navigator, clipboard: undefined });
    expect(await copyPicture(photo({ mime: "image/png" }))).toBe(false);
    expect(await copyPicture(photo({ kind: "video", urls: {} }))).toBe(false);
  });

  it("hands on the picture as a file named after the photo", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => picture("image/jpeg")));
    const file = await pictureFile(photo({ mime: "image/heic" }));
    expect(file?.name).toBe("IMG_0001.jpg");
    expect(file?.type).toBe("image/jpeg");
  });
});
