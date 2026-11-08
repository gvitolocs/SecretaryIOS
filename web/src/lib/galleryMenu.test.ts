import { describe, expect, it, vi } from "vitest";
import type { GalleryItem } from "../api/gallery";
import { galleryActions, galleryMenuTitle, type GalleryMenuEnv } from "./galleryMenu";

const item = (id: string, over: Partial<GalleryItem> = {}): GalleryItem =>
  ({
    id, kind: "photo", title: `${id}.jpg`, mime: "image/jpeg", size: 1000, favorite: false, edited: false,
    origin: { filename: `${id}.jpg`, folder: null, importedFrom: null },
    urls: { original: `/v1/media/items/${id}/original`, large: `/v1/media/items/${id}/large` },
    takenAt: "2026-10-29T10:00:00Z", addedAt: "2026-10-29T10:00:00Z",
    ...over,
  }) as GalleryItem;

function env(over: Partial<GalleryMenuEnv> = {}): GalleryMenuEnv {
  const fn = () => vi.fn();
  return {
    open: fn(), copyImage: fn(), copyLink: fn(), download: fn(), share: fn(), favorite: fn(), showDay: fn(), askLucina: fn(), select: fn(), remove: fn(),
    ...over,
  };
}
const ids = (targets: GalleryItem[], e = env()) => galleryActions(targets, e).map((a) => a.id);

describe("galleryActions", () => {
  it("offers everything for one photo, in order", () => {
    expect(ids([item("a")])).toEqual(["open", "copyImage", "copyLink", "download", "share", "favorite", "showDay", "askLucina", "delete"]);
  });

  it("drops Open in the viewer, and offers Select only on the phone sheet for an unselected photo", () => {
    expect(ids([item("a")], env({ viewer: true }))[0]).toBe("copyImage");
    expect(ids([item("a")], env({ sheet: true }))).toContain("select");
    expect(ids([item("a")], env({ sheet: true, isSelected: () => true }))).not.toContain("select");
  });

  it("copies a video's frame only when it has a poster, and never asks Lucina about a video", () => {
    const video = item("v", { kind: "video", mime: "video/mp4" });
    expect(ids([video])).not.toContain("copyImage");
    const withPoster = item("v", { kind: "video", mime: "video/mp4", urls: { poster: "/p.jpg" } });
    const copy = galleryActions([withPoster], env()).find((a) => a.id === "copyImage")!;
    expect(copy.label).toBe("Copy frame");
    expect(ids([withPoster])).not.toContain("askLucina");
  });

  it("acts on all of several photos: no Open, Copy image, Show in its day or Ask Lucina", () => {
    const e = env();
    const targets = [item("a"), item("b")];
    expect(ids(targets, e)).toEqual(["copyLink", "download", "share", "favorite", "delete"]);
    galleryActions(targets, e).find((a) => a.id === "delete")!.run();
    expect(e.remove).toHaveBeenCalledWith(targets);
    expect(galleryActions(targets, e).find((a) => a.id === "copyLink")!.label).toBe("Copy links");
  });

  it("says Unfavorite when every photo is a favorite already", () => {
    const e = env();
    const fav = galleryActions([item("a", { favorite: true }), item("b", { favorite: true })], e).find((a) => a.id === "favorite")!;
    expect(fav.label).toBe("Unfavorite");
    fav.run();
    expect(e.favorite).toHaveBeenCalledWith(expect.any(Array), false);
    expect(galleryActions([item("a", { favorite: true }), item("b")], env()).find((a) => a.id === "favorite")!.label).toBe("Favorite");
  });

  it("keeps the Trash to Open, Copy link, Restore and Delete forever", () => {
    const actions = galleryActions([item("a")], env({ trash: true, restore: vi.fn() }));
    expect(actions.map((a) => a.id)).toEqual(["open", "copyLink", "restore", "delete"]);
    expect(actions.at(-1)!.label).toBe("Delete forever");
  });

  it("offers Unarchive instead of Favorite in the Archive", () => {
    const actions = galleryActions([item("a")], env({ archive: true, archiveToggle: vi.fn() }));
    expect(actions.map((a) => a.id)).not.toContain("favorite");
    expect(actions.find((a) => a.id === "archive")!.label).toBe("Unarchive");
  });

  it("marks Delete as dangerous and disables Download when nothing can be downloaded", () => {
    const actions = galleryActions([item("a", { urls: {} })], env());
    expect(actions.find((a) => a.id === "delete")!.danger).toBe(true);
    expect(actions.find((a) => a.id === "download")!.disabled).toBe(true);
  });

  it("titles the sheet with the photo's name, or the count", () => {
    expect(galleryMenuTitle([item("a")])).toBe("a.jpg");
    expect(galleryMenuTitle([item("a"), item("b")])).toBe("2 selected");
  });
});
