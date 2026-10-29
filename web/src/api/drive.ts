// The Drive on lucina-media (/v1/media/drive, docs/media-library.md): same-origin through p1,
// parsed defensively by lib/drive.ts. An older server has no /drive endpoints: the roots fall
// back to the Drive albums (`/albums?scope=drive`).

import { ApiError, request } from "./client";
import {
  browseUrl,
  type DriveFilters,
  type DriveItem,
  type DriveRoots,
  type DriveType,
  itemsUrl,
  parseAlbums,
  parseArchive,
  parseBrowse,
  parseDriveItem,
  parseItemsPage,
  parseRoots,
  parseTextPreview,
  type ArchiveListing,
  type BrowsePage,
  type ItemsPage,
  type TextPreview,
} from "../lib/drive";

export const driveApi = {
  /** The Drive roots: folders on disk and other Drive albums. `legacy` when the server has no /drive. */
  async roots(): Promise<DriveRoots & { legacy: boolean }> {
    try {
      return { ...parseRoots(await request<unknown>("GET", "/v1/media/drive")), legacy: false };
    } catch (e) {
      if (!(e instanceof ApiError) || (e.status !== 404 && e.status !== 405)) throw e;
      const albums = await request<{ albums?: unknown }>("GET", "/v1/media/albums?scope=drive");
      return { folders: [], albums: parseAlbums(albums?.albums).filter((a) => a.count !== 0), converters: {}, legacy: true };
    }
  },

  async browse(folder: string, path: string, filters: DriveFilters, offset = 0): Promise<BrowsePage> {
    return parseBrowse(await request<unknown>("GET", browseUrl(folder, path, filters, offset)), folder, path);
  },

  async items(opts: { q?: string; type?: DriveType | null; album?: string; cursor?: string | null }): Promise<ItemsPage> {
    return parseItemsPage(await request<unknown>("GET", itemsUrl(opts)));
  },

  async item(id: string): Promise<DriveItem | null> {
    return parseDriveItem(await request<unknown>("GET", `/v1/media/items/${encodeURIComponent(id)}`));
  },

  /** A text/code file's text, highlighted. `{state: "pending"}` (202) while the server extracts it. */
  async text(url: string, highlight = true): Promise<TextPreview> {
    const sep = url.includes("?") ? "&" : "?";
    return parseTextPreview(await request<unknown>("GET", highlight ? `${url}${sep}highlight=1` : url, { timeoutMs: 60_000 }));
  },

  async archive(url: string): Promise<ArchiveListing> {
    return parseArchive(await request<unknown>("GET", url, { timeoutMs: 60_000 }));
  },
};
