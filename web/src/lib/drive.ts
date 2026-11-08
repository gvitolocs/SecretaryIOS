// The Drive file browser (views/Drive.tsx): every file in the owner's Drive folders on
// lucina-media (docs/media-library.md, the Drive API). Pure logic only: parsing the server's
// JSON defensively (every new field may be missing on an older server), same-origin URLs
// only, routes, queries, breadcrumbs and the type → icon/label mapping.

import { sameOriginPath } from "../api/media";

export const DRIVE_TYPES = [
  "pdf",
  "document",
  "spreadsheet",
  "presentation",
  "text",
  "code",
  "archive",
  "image",
  "video",
  "audio",
  "other",
] as const;
export type DriveType = (typeof DRIVE_TYPES)[number];

export const DRIVE_PREVIEWS = ["pdf", "office", "text", "code", "archive", "image", "video", "audio", "none"] as const;
export type DrivePreview = (typeof DRIVE_PREVIEWS)[number];

export const DRIVE_SORTS = ["name", "modified", "size", "type"] as const;
export type DriveSort = (typeof DRIVE_SORTS)[number];
export type DriveOrder = "asc" | "desc";

export interface DriveInfo {
  folder: string;
  path: string;
  dir: string;
  name: string;
  ext: string;
  type: DriveType;
  preview: DrivePreview;
  language: string | null;
  modified: string | null;
  text: string | null;
}

export interface DriveUrls {
  original?: string;
  download?: string;
  thumb?: string;
  large?: string;
  grid?: string;
  preview?: string;
  poster?: string;
  play?: string;
  pdf?: string;
  text?: string;
  archive?: string;
}

const URL_KEYS = ["original", "download", "thumb", "large", "grid", "preview", "poster", "play", "pdf", "text", "archive"] as const;

export interface DriveItem {
  id: string;
  kind: string;
  title: string;
  mime: string;
  size: number | null;
  createdAt: string | null;
  takenAt: string | null;
  thumbState: string | null;
  durationMs: number | null;
  urls: DriveUrls;
  /** Tokenised `/v1/media/s/...` URLs (no login check, no rate limit). */
  thumbUrl: string | null;
  posterUrl: string | null;
  album: string | null;
  drive: DriveInfo | null;
  snippet: string | null;
}

export interface DriveRoot {
  folder: string;
  files: number | null;
  bytes: number | null;
  modified: string | null;
}

export interface DriveAlbum {
  id: string;
  title: string;
  count: number | null;
}

export interface DriveRoots {
  folders: DriveRoot[];
  albums: DriveAlbum[];
  converters: Record<string, boolean>;
}

export interface Crumb {
  name: string;
  path: string;
}

export interface DriveFolderEntry {
  name: string;
  path: string;
  files: number | null;
  bytes: number | null;
  modified: string | null;
}

export interface BrowsePage {
  folder: string;
  path: string;
  breadcrumbs: Crumb[];
  folders: DriveFolderEntry[];
  files: DriveItem[];
  total: number | null;
  nextOffset: number | null;
}

export interface ItemsPage {
  items: DriveItem[];
  nextCursor: string | null;
  total: number | null;
}

export type Token = [cls: string, text: string];

export type TextPreview =
  | { state: "pending" }
  | {
      state: "ready";
      language: string | null;
      source: string | null;
      truncated: boolean;
      bytes: number | null;
      text: string;
      lines: Token[][] | null;
    };

export interface ArchiveEntry {
  path: string;
  size: number | null;
  dir: boolean;
  modified: string | null;
}

export interface ArchiveListing {
  format: string | null;
  total: number | null;
  truncated: boolean;
  entries: ArchiveEntry[];
}

// ---------------------------------------------------------------- parsing helpers

type Obj = Record<string, unknown>;

function obj(v: unknown): Obj | null {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Obj) : null;
}

function str(v: unknown, max = 4096): string | null {
  return typeof v === "string" ? v.slice(0, max) : null;
}

function nonEmpty(v: unknown, max = 4096): string | null {
  const s = str(v, max);
  return s ? s : null;
}

function count(v: unknown): number | null {
  return typeof v === "number" && Number.isFinite(v) && v >= 0 ? v : null;
}

function oneOf<T extends string>(v: unknown, values: readonly T[]): T | null {
  return typeof v === "string" && (values as readonly string[]).includes(v) ? (v as T) : null;
}

/** A Drive path within a folder: '/'-separated, no leading '/', no `.`/`..`/empty segments. */
export function cleanPath(v: unknown): string {
  if (typeof v !== "string") return "";
  return v
    .split("/")
    .filter((s) => s !== "" && s !== "." && s !== ".." && !s.includes("\\") && !s.includes("\0"))
    .join("/");
}

export function extOf(name: string): string {
  const base = name.split("/").pop() ?? "";
  const dot = base.lastIndexOf(".");
  return dot > 0 ? base.slice(dot + 1).toLowerCase() : "";
}

const EXT_TYPES: Record<string, DriveType> = {
  pdf: "pdf",
  doc: "document", docx: "document", odt: "document", rtf: "document", pages: "document",
  xls: "spreadsheet", xlsx: "spreadsheet", ods: "spreadsheet", csv: "spreadsheet", tsv: "spreadsheet", numbers: "spreadsheet",
  ppt: "presentation", pptx: "presentation", odp: "presentation", key: "presentation",
  txt: "text", md: "text", log: "text", rst: "text", tex: "text",
  rs: "code", py: "code", js: "code", ts: "code", tsx: "code", jsx: "code", c: "code", h: "code", cpp: "code", hpp: "code",
  java: "code", kt: "code", swift: "code", go: "code", rb: "code", sh: "code", json: "code", toml: "code", yaml: "code",
  yml: "code", xml: "code", html: "code", css: "code", sql: "code", m: "code", r: "code", ipynb: "code",
  zip: "archive", tar: "archive", gz: "archive", tgz: "archive", "7z": "archive", rar: "archive", bz2: "archive", xz: "archive",
};

/** The Drive type of an item: the server's, else guessed from kind, MIME and extension (older servers). */
export function guessType(kind: string, mime: string, ext: string): DriveType {
  if (kind === "photo" || mime.startsWith("image/")) return "image";
  if (kind === "video" || mime.startsWith("video/")) return "video";
  if (kind === "audio" || mime.startsWith("audio/")) return "audio";
  if (mime === "application/pdf") return "pdf";
  return EXT_TYPES[ext] ?? (mime.startsWith("text/") ? "text" : "other");
}

export function previewForType(type: DriveType): DrivePreview {
  switch (type) {
    case "pdf":
    case "text":
    case "code":
    case "archive":
    case "image":
    case "video":
    case "audio":
      return type;
    case "document":
    case "spreadsheet":
    case "presentation":
      return "office";
    default:
      return "none";
  }
}

function parseDriveInfo(raw: unknown, title: string): DriveInfo | null {
  const d = obj(raw);
  if (!d) return null;
  const folder = nonEmpty(d.folder, 512);
  if (!folder) return null;
  const path = cleanPath(d.path);
  const name = nonEmpty(d.name, 1024) ?? (path.split("/").pop() || title);
  const ext = typeof d.ext === "string" ? d.ext.toLowerCase().slice(0, 32) : extOf(name);
  const type = oneOf(d.type, DRIVE_TYPES) ?? guessType("file", "", ext);
  const slash = path.lastIndexOf("/");
  const dir = typeof d.dir === "string" ? cleanPath(d.dir) : slash > 0 ? path.slice(0, slash) : "";
  return {
    folder,
    path,
    dir,
    name,
    ext,
    type,
    preview: oneOf(d.preview, DRIVE_PREVIEWS) ?? previewForType(type),
    language: nonEmpty(d.language, 64),
    modified: nonEmpty(d.modified, 64),
    text: nonEmpty(d.text, 32),
  };
}

/** An item from `/items`, `/items/{id}` or `/drive/browse`; null when it has no usable id. */
export function parseDriveItem(raw: unknown): DriveItem | null {
  const b = obj(raw);
  if (!b) return null;
  const id = nonEmpty(b.id, 128);
  if (!id) return null;
  const urls: DriveUrls = {};
  const u = obj(b.urls);
  if (u) {
    for (const key of URL_KEYS) {
      const value = sameOriginPath(u[key]);
      if (value) urls[key] = value;
    }
  }
  const title = nonEmpty(b.title, 1024) ?? id;
  const snippet = nonEmpty(b.snippet, 2000);
  return {
    id,
    kind: nonEmpty(b.kind, 32) ?? "file",
    title,
    mime: nonEmpty(b.mime, 255) ?? "",
    size: count(b.size),
    createdAt: nonEmpty(b.created_at, 64),
    takenAt: nonEmpty(b.taken_at, 64),
    thumbState: nonEmpty(b.thumb_state, 32),
    durationMs: count(b.duration_ms),
    urls,
    thumbUrl: sameOriginPath(b.thumb_url) ?? null,
    posterUrl: sameOriginPath(b.poster_url) ?? null,
    album: nonEmpty(b.album, 512),
    drive: parseDriveInfo(b.drive, title),
    snippet,
  };
}

function parseItems(v: unknown): DriveItem[] {
  return Array.isArray(v) ? v.map(parseDriveItem).filter((x): x is DriveItem => x !== null) : [];
}

export function itemType(item: DriveItem): DriveType {
  return item.drive?.type ?? guessType(item.kind, item.mime, extOf(item.title));
}

export function itemPreview(item: DriveItem): DrivePreview {
  return item.drive?.preview ?? previewForType(itemType(item));
}

export function itemName(item: DriveItem): string {
  return item.drive?.name || item.title;
}

export function itemModified(item: DriveItem): string | null {
  return item.drive?.modified ?? item.takenAt ?? item.createdAt;
}

/** "Lezioni / Digital Forensics": where a file lives, for search results. */
export function itemLocation(item: DriveItem): string {
  if (!item.drive) return item.album ?? "";
  return [item.drive.folder, ...item.drive.dir.split("/").filter(Boolean)].join(" / ");
}

/** The thumbnail to show in a row: tokenised JPEG first, then JXL grid where it decodes, then the JPEG thumb. */
export function thumbnailUrl(item: DriveItem, jxl: boolean): string | null {
  if (item.thumbState && item.thumbState !== "ready") return null;
  return item.thumbUrl ?? (jxl ? item.urls.grid : undefined) ?? item.urls.thumb ?? null;
}

/** The image to show in a preview: the tokenised original (as the photo viewer does), else large, else original. */
export function imageUrl(item: DriveItem): string | null {
  const token = item.thumbUrl?.match(/^\/v1\/media\/s\/[^/]+\/[^/]+\//)?.[0];
  if (token) return `${token}file`;
  return item.urls.large ?? item.urls.original ?? null;
}

export function downloadUrl(item: DriveItem): string {
  return item.urls.download ?? `/v1/media/items/${encodeURIComponent(item.id)}/original?download=1`;
}

export function parseRoots(raw: unknown): DriveRoots {
  const b = obj(raw) ?? {};
  const folders: DriveRoot[] = [];
  if (Array.isArray(b.folders)) {
    for (const f of b.folders) {
      const o = obj(f);
      const folder = o && nonEmpty(o.folder, 512);
      if (!o || !folder || folder.includes("/")) continue;
      folders.push({ folder, files: count(o.files), bytes: count(o.bytes), modified: nonEmpty(o.modified, 64) });
    }
  }
  const converters: Record<string, boolean> = {};
  const c = obj(b.converters);
  if (c) for (const [k, v] of Object.entries(c)) if (typeof v === "boolean") converters[k] = v;
  return { folders, albums: parseAlbums(b.albums), converters };
}

export function parseAlbums(v: unknown): DriveAlbum[] {
  if (!Array.isArray(v)) return [];
  const out: DriveAlbum[] = [];
  for (const a of v) {
    const o = obj(a);
    const id = o && nonEmpty(o.id, 128);
    if (!o || !id) continue;
    out.push({ id, title: nonEmpty(o.title, 512) ?? id, count: count(o.count) });
  }
  return out;
}

export function parseBrowse(raw: unknown, folder: string, path: string): BrowsePage {
  const b = obj(raw) ?? {};
  const realFolder = nonEmpty(b.folder, 512) ?? folder;
  const realPath = typeof b.path === "string" ? cleanPath(b.path) : path;
  let crumbs: Crumb[] = [];
  if (Array.isArray(b.breadcrumbs)) {
    for (const c of b.breadcrumbs) {
      const o = obj(c);
      const name = o && nonEmpty(o.name, 1024);
      if (!o || !name) continue;
      crumbs.push({ name, path: cleanPath(o.path) });
    }
  }
  if (crumbs.length === 0) crumbs = breadcrumbs(realFolder, realPath);
  const folders: DriveFolderEntry[] = [];
  if (Array.isArray(b.folders)) {
    for (const f of b.folders) {
      const o = obj(f);
      if (!o) continue;
      const p = cleanPath(o.path);
      const name = nonEmpty(o.name, 1024) ?? p.split("/").pop() ?? "";
      if (!p || !name) continue;
      folders.push({ name, path: p, files: count(o.files), bytes: count(o.bytes), modified: nonEmpty(o.modified, 64) });
    }
  }
  const next = count(b.next_offset);
  return {
    folder: realFolder,
    path: realPath,
    breadcrumbs: crumbs,
    folders,
    files: parseItems(b.files),
    total: count(b.total),
    nextOffset: next === null ? null : Math.floor(next),
  };
}

export function parseItemsPage(raw: unknown): ItemsPage {
  const b = obj(raw) ?? {};
  return { items: parseItems(b.items), nextCursor: nonEmpty(b.next_cursor, 512), total: count(b.total) };
}

const TOKEN_CLASSES = new Set(["kw", "str", "com", "num", "fn", "type", "const", "punct", "tag", "attr", "var", "head", "em"]);

/** A token class as a CSS class (`tok-kw`), or "" for plain text and unknown classes. */
export function tokenClass(cls: string): string {
  return TOKEN_CLASSES.has(cls) ? `tok-${cls}` : "";
}

function parseLines(v: unknown): Token[][] | null {
  if (!Array.isArray(v)) return null;
  const lines: Token[][] = [];
  for (const line of v) {
    if (!Array.isArray(line)) {
      lines.push([]);
      continue;
    }
    const tokens: Token[] = [];
    for (const tok of line) {
      if (Array.isArray(tok) && typeof tok[1] === "string") tokens.push([typeof tok[0] === "string" ? tok[0] : "", tok[1]]);
      else if (typeof tok === "string") tokens.push(["", tok]);
    }
    lines.push(tokens);
  }
  return lines;
}

/** `GET urls.text[?highlight=1]`: 202 `{"state":"pending"}` while the server extracts the text. */
export function parseTextPreview(raw: unknown): TextPreview {
  const b = obj(raw) ?? {};
  if (b.state === "pending") return { state: "pending" };
  return {
    state: "ready",
    language: nonEmpty(b.language, 64),
    source: nonEmpty(b.source, 32),
    truncated: b.truncated === true,
    bytes: count(b.bytes),
    text: typeof b.text === "string" ? b.text : "",
    lines: parseLines(b.lines),
  };
}

/** The lines to render: the highlighted ones, then the rest of the text plain (highlighting stops at 5,000 lines). */
export function previewLines(p: Extract<TextPreview, { state: "ready" }>): Token[][] {
  const plain = p.text.split(/\r?\n/);
  if (plain.length > 1 && plain[plain.length - 1] === "") plain.pop();
  const lines = p.lines ?? [];
  const out = lines.slice();
  for (let i = lines.length; i < plain.length; i++) out.push([["", plain[i]!]]);
  return out;
}

export function parseArchive(raw: unknown): ArchiveListing {
  const b = obj(raw) ?? {};
  const entries: ArchiveEntry[] = [];
  if (Array.isArray(b.entries)) {
    for (const e of b.entries) {
      const o = obj(e);
      const path = o && nonEmpty(o.path, 4096);
      if (!o || !path) continue;
      entries.push({ path, size: count(o.size), dir: o.dir === true, modified: nonEmpty(o.modified, 64) });
    }
  }
  return { format: nonEmpty(b.format, 32), total: count(b.total), truncated: b.truncated === true, entries };
}

// ---------------------------------------------------------------- breadcrumbs, queries

/** The crumbs from the folder root down to `path` (when the server sends none). */
export function breadcrumbs(folder: string, path: string): Crumb[] {
  const crumbs: Crumb[] = [{ name: folder, path: "" }];
  const parts = cleanPath(path).split("/").filter(Boolean);
  parts.forEach((name, i) => crumbs.push({ name, path: parts.slice(0, i + 1).join("/") }));
  return crumbs;
}

export interface DriveFilters {
  q: string;
  type: DriveType | null;
  sort: DriveSort | null;
  order: DriveOrder | null;
}

export const NO_FILTERS: DriveFilters = { q: "", type: null, sort: null, order: null };

/** The server's default order for a sort: ascending for name and type, descending for modified and size. */
export function defaultOrder(sort: DriveSort): DriveOrder {
  return sort === "modified" || sort === "size" ? "desc" : "asc";
}

export const BROWSE_LIMIT = 200;

/** `/v1/media/drive/browse?…` for a folder (and its subtree when searching). */
export function browseUrl(folder: string, path: string, f: DriveFilters, offset = 0, limit = BROWSE_LIMIT): string {
  const q = new URLSearchParams({ folder });
  const p = cleanPath(path);
  if (p) q.set("path", p);
  if (f.sort) q.set("sort", f.sort);
  if (f.sort && f.order) q.set("order", f.order);
  if (f.type) q.set("type", f.type);
  const words = f.q.trim();
  if (words) q.set("q", words);
  if (offset > 0) q.set("offset", String(offset));
  q.set("limit", String(limit));
  return `/v1/media/drive/browse?${q}`;
}

/** `/v1/media/items?scope=drive&…`: search everywhere, or an album's items. */
export function itemsUrl(opts: { q?: string; type?: DriveType | null; album?: string; cursor?: string | null; limit?: number }): string {
  const q = new URLSearchParams({ scope: "drive" });
  if (opts.album) q.set("album", opts.album);
  const words = opts.q?.trim();
  if (words) q.set("q", words);
  if (opts.type) q.set("type", opts.type);
  if (opts.cursor) q.set("cursor", opts.cursor);
  q.set("limit", String(opts.limit ?? BROWSE_LIMIT));
  return `/v1/media/items?${q}`;
}

// ---------------------------------------------------------------- routes

export type DriveRoute =
  | { view: "roots" }
  | { view: "folder"; folder: string; path: string }
  | { view: "album"; id: string }
  | { view: "file"; id: string };

/** Percent-encode a path segment, also the characters chat route buttons do not allow (`!'()*~`). */
export function encodeSegment(s: string): string {
  return encodeURIComponent(s).replace(/[!'()*~]/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`);
}

function decodeSegment(s: string): string | null {
  try {
    return decodeURIComponent(s);
  } catch {
    return null;
  }
}

/** `/drive`, `/drive/folder/<folder>/<path…>`, `/drive/album/<id>`, `/drive/file/<id>`. */
export function parseDriveRoute(pathname: string): DriveRoute {
  const raw = pathname.split("?")[0]!.split("#")[0]!.split("/").filter(Boolean);
  if (raw[0] !== "drive") return { view: "roots" };
  const parts: string[] = [];
  for (const s of raw.slice(1)) {
    const d = decodeSegment(s);
    if (d === null) return { view: "roots" };
    parts.push(d);
  }
  const [kind, first] = parts;
  if (kind === "folder" && first && !first.includes("/") && first !== "." && first !== "..") {
    return { view: "folder", folder: first, path: cleanPath(parts.slice(2).join("/")) };
  }
  if ((kind === "file" || kind === "album") && first) return { view: kind, id: first };
  return { view: "roots" };
}

export function folderHref(folder: string, path = ""): string {
  const segs = [folder, ...cleanPath(path).split("/").filter(Boolean)].map(encodeSegment);
  return `/drive/folder/${segs.join("/")}`;
}

export function fileHref(id: string): string {
  return `/drive/file/${encodeSegment(id)}`;
}

/** The sheet editor (views/Sheet.tsx, docs/xlsx.md). */
export function sheetHref(id: string): string {
  return `/drive/sheet/${encodeSegment(id)}`;
}

/** Workbooks the sheet editor opens (lucina-media `sheet_ext`); .xls, .ods and .numbers only preview. */
export function opensAsSheet(item: DriveItem): boolean {
  return item.kind === "file" && /\.(xlsx|xlsm|xltx|xltm)$/i.test(itemName(item));
}

export function albumHref(id: string): string {
  return `/drive/album/${encodeSegment(id)}`;
}

/** The view's filters from the location's search string (`?q=&type=&sort=&order=`). */
export function parseFilters(search: string): DriveFilters {
  const p = new URLSearchParams(search);
  const sort = oneOf(p.get("sort"), DRIVE_SORTS);
  const order = oneOf(p.get("order"), ["asc", "desc"] as const);
  return { q: (p.get("q") ?? "").slice(0, 200), type: oneOf(p.get("type"), DRIVE_TYPES), sort, order: sort ? order : null };
}

/** The search string for filters (empty values left out), plus an open preview's id. */
export function filtersSearch(f: DriveFilters, open?: string | null): string {
  const p = new URLSearchParams();
  if (f.q.trim()) p.set("q", f.q.trim());
  if (f.type) p.set("type", f.type);
  if (f.sort) p.set("sort", f.sort);
  if (f.sort && f.order) p.set("order", f.order);
  if (open) p.set("open", open);
  const s = p.toString();
  return s ? `?${s}` : "";
}

export function openParam(search: string): string | null {
  const v = new URLSearchParams(search).get("open");
  return v && v.length <= 128 ? v : null;
}

// ---------------------------------------------------------------- labels and icons

export type DriveTypeKey =
  | "typePdf"
  | "typeDocument"
  | "typeSpreadsheet"
  | "typePresentation"
  | "typeText"
  | "typeCode"
  | "typeArchive"
  | "typeImage"
  | "typeVideo"
  | "typeAudio"
  | "typeOther";

/** The i18n key (`drive.<key>`) naming a type, singular ("PDF", "Spreadsheet"). */
export function typeLabelKey(type: DriveType): DriveTypeKey {
  return `type${type.charAt(0).toUpperCase()}${type.slice(1)}` as DriveTypeKey;
}

/** The i18n key of a filter chip (plural: "Documents", "Slides"). */
export function filterLabelKey(type: DriveType): `filter${Capitalize<DriveType>}` {
  return `filter${type.charAt(0).toUpperCase()}${type.slice(1)}` as `filter${Capitalize<DriveType>}`;
}

/** The short badge drawn on a file icon, and the icon's colour family. */
export function typeBadge(type: DriveType, ext = ""): { badge: string; tone: string } {
  switch (type) {
    case "pdf":
      return { badge: "PDF", tone: "red" };
    case "document":
      return { badge: "DOC", tone: "blue" };
    case "spreadsheet":
      return { badge: "XLS", tone: "green" };
    case "presentation":
      return { badge: "PPT", tone: "orange" };
    case "text":
      return { badge: ext === "md" ? "MD" : "TXT", tone: "gray" };
    case "code":
      return { badge: ext && ext.length <= 4 ? ext.toUpperCase() : "</>", tone: "purple" };
    case "archive":
      return { badge: "ZIP", tone: "yellow" };
    case "image":
      return { badge: "IMG", tone: "teal" };
    case "video":
      return { badge: "VID", tone: "pink" };
    case "audio":
      return { badge: "AUD", tone: "indigo" };
    default:
      return { badge: ext && ext.length <= 4 ? ext.toUpperCase() : "", tone: "gray" };
  }
}

const UNITS = ["B", "KB", "MB", "GB", "TB"];

/** "1.2 MB" (decimal units, the language's number format). */
export function formatBytes(n: number | null | undefined, lang = "en"): string {
  if (n === null || n === undefined || !Number.isFinite(n) || n < 0) return "";
  let v = n;
  let u = 0;
  while (v >= 1000 && u < UNITS.length - 1) {
    v /= 1000;
    u++;
  }
  const digits = u === 0 || v >= 100 ? 0 : 1;
  return `${v.toLocaleString(lang, { maximumFractionDigits: digits, minimumFractionDigits: 0 })} ${UNITS[u]}`;
}

/** A modified time as a short date ("4 Mar 2026"), or "" when missing or invalid. */
export function formatModified(iso: string | null | undefined, lang = "en"): string {
  if (!iso) return "";
  const ms = Date.parse(iso);
  if (Number.isNaN(ms)) return "";
  return new Date(ms).toLocaleDateString(lang, { day: "numeric", month: "short", year: "numeric" });
}

/** Files at most this big are shared as files (Web Share); bigger ones as a link. */
export const SHARE_FILE_LIMIT = 25_000_000;
