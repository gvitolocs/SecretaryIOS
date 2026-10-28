// The albums page's rules, with no DOM: which albums are listed, in which section, and where an album lives.

import type { AlbumCard } from "../api/gallery";

/** `system_key` prefix of the hidden album behind a photo-selection share: never the owner's, never listed. */
export const SHARE_ALBUM_PREFIX = "share:";

export const isShareAlbum = (a: Pick<AlbumCard, "systemKey">): boolean => !!a.systemKey && a.systemKey.startsWith(SHARE_ALBUM_PREFIX);

/** A built-in album (Lucina's): shown, but it can't be renamed or deleted. */
export const isBuiltIn = (a: Pick<AlbumCard, "systemKey">): boolean => !!a.systemKey && !isShareAlbum(a);

export interface AlbumSections {
  /** The owner's albums, most recently changed first. */
  own: AlbumCard[];
  /** The built-in ones, after the owner's. */
  builtIn: AlbumCard[];
}

const byNewest = (a: AlbumCard, b: AlbumCard): number => (a.updatedAt === b.updatedAt ? a.title.localeCompare(b.title) : a.updatedAt < b.updatedAt ? 1 : -1);

/** Hide the share albums and split the rest into the owner's and the built-in ones. */
export function sectionAlbums(all: readonly AlbumCard[]): AlbumSections {
  const visible = all.filter((a) => !isShareAlbum(a));
  return { own: visible.filter((a) => !a.systemKey).sort(byNewest), builtIn: visible.filter((a) => !!a.systemKey).sort((a, b) => a.title.localeCompare(b.title)) };
}

export const albumPath = (id: string): string => `/gallery/album/${encodeURIComponent(id)}`;
export const ALBUMS_PATH = "/gallery/albums";
