import { describe, expect, it } from "vitest";
import type { AlbumCard } from "../api/gallery";
import { albumPath, isBuiltIn, isShareAlbum, sectionAlbums } from "./galleryAlbums";

const album = (id: string, over: Partial<AlbumCard> = {}): AlbumCard => ({
  id,
  title: id,
  systemKey: null,
  coverItem: null,
  count: 3,
  createdAt: "2026-10-01T00:00:00Z",
  updatedAt: "2026-10-01T00:00:00Z",
  ...over,
});

describe("sectionAlbums", () => {
  it("hides the albums behind share links", () => {
    const all = [album("mine"), album("hidden", { systemKey: "share:abc123" }), album("lucina", { systemKey: "lucina" })];
    const { own, builtIn } = sectionAlbums(all);
    expect(own.map((a) => a.id)).toEqual(["mine"]);
    expect(builtIn.map((a) => a.id)).toEqual(["lucina"]);
    expect([...own, ...builtIn].some(isShareAlbum)).toBe(false);
  });

  it("puts the owner's albums first, newest change first, and the built-in ones in their own section", () => {
    const { own, builtIn } = sectionAlbums([
      album("old", { updatedAt: "2026-01-01T00:00:00Z" }),
      album("sys-b", { systemKey: "b", title: "Zeta" }),
      album("new", { updatedAt: "2026-10-01T00:00:00Z" }),
      album("sys-a", { systemKey: "a", title: "Alpha" }),
    ]);
    expect(own.map((a) => a.id)).toEqual(["new", "old"]);
    expect(builtIn.map((a) => a.id)).toEqual(["sys-a", "sys-b"]);
  });

  it("keeps empty albums (a new album has no photos yet)", () => {
    expect(sectionAlbums([album("fresh", { count: 0 })]).own).toHaveLength(1);
  });

  it("tells built-in from share albums", () => {
    expect(isBuiltIn({ systemKey: "lucina" })).toBe(true);
    expect(isBuiltIn({ systemKey: "share:x" })).toBe(false);
    expect(isBuiltIn({ systemKey: null })).toBe(false);
  });
});

describe("albumPath", () => {
  it("is the route Lucina's chat links to", () => {
    expect(albumPath("alb_1")).toBe("/gallery/album/alb_1");
    expect(albumPath("a b/c")).toBe("/gallery/album/a%20b%2Fc");
  });
});
