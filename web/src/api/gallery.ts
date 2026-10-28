// The Gallery's client for lucina-media (docs/media-library.md, API): the phones', the Mac's, the old backups' and the
// chat's photos and videos, per source, with their details, edits and the trash. Drive files are not here.
//
// Deleting in Lucina never deletes the original on the device; only `alsoFromDevice` asks the device to (it then asks
// the system, which confirms on the device).

import { request } from "./client";
import { sameOriginPath } from "./media";

export type GalleryKind = "photo" | "video";
export type Rotation = 0 | 90 | 180 | 270;

export interface GalleryPlace {
  name: string;
  admin1?: string | null;
  admin2?: string | null;
  country?: string | null;
  lat: number;
  lon: number;
}

export interface GalleryUrls {
  thumb?: string;
  large?: string;
  grid?: string;
  preview?: string;
  original?: string;
  download?: string;
  poster?: string;
  play?: string;
}

export interface GalleryOrigin {
  filename: string | null;
  folder: string | null;
  importedFrom: string | null;
}

export interface GalleryItem {
  id: string;
  kind: GalleryKind;
  title: string;
  caption: string | null;
  mime: string;
  size: number;
  takenAt: string | null;
  addedAt: string;
  width: number | null;
  height: number | null;
  durationMs: number | null;
  place: GalleryPlace | null;
  camera: string | null;
  source: string;
  device: string | null;
  deviceName: string | null;
  origin: GalleryOrigin;
  favorite: boolean;
  rotation: Rotation;
  albums: string[];
  trashedAt: string | null;
  trashExpiresAt: string | null;
  thumbhash: string | null;
  urls: GalleryUrls;
}

export type SourceKind = "iphone" | "ipad" | "android" | "mac" | "pc" | "backup" | "camera" | "other" | "chat" | "lucina";

export interface GallerySource {
  /** `device:<id>`, `chat` or `lucina`. */
  id: string;
  label: string;
  kind: SourceKind;
  photos: number;
  videos: number;
  latestTakenAt: string | null;
}

export interface GalleryQuery {
  /** A source id, or `all` (the whole Gallery). */
  origin?: string;
  kind?: GalleryKind | "all";
  from?: string;
  to?: string;
  q?: string;
  favorite?: boolean;
  trashed?: boolean;
  album?: string;
  cursor?: string;
  limit?: number;
}

export interface GalleryPage {
  items: GalleryItem[];
  nextCursor: string | null;
}

export interface Exif {
  make: string | null;
  model: string | null;
  lens: string | null;
  iso: number | null;
  exposureS: number | null;
  fNumber: number | null;
  focalMm: number | null;
  flash: boolean | null;
  orientation: number | null;
  software: string | null;
  takenAt: string | null;
  offset: string | null;
}

export interface ItemDetails {
  item: GalleryItem;
  exif: Exif | null;
  people: string[];
}

export type BatchOp = "trash" | "restore" | "purge" | "favorite" | "unfavorite" | "album_add" | "album_remove";

export interface BatchResult {
  done: string[];
  failed: { id: string; error: string }[];
}

export interface GalleryAlbum {
  id: string;
  title: string;
  count: number;
  system: boolean;
}

// ---- parsing (the server's JSON is snake_case; anything malformed is dropped) ---------------------------------------

type Raw = Record<string, unknown>;

const obj = (v: unknown): Raw => (v && typeof v === "object" && !Array.isArray(v) ? (v as Raw) : {});
const str = (v: unknown): string | null => (typeof v === "string" && v ? v : null);
const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);
const ROTATIONS = new Set([0, 90, 180, 270]);

function parsePlace(v: unknown): GalleryPlace | null {
  const p = obj(v);
  const lat = num(p.lat);
  const lon = num(p.lon);
  if (lat === null || lon === null) return null;
  return { name: str(p.name) ?? "", admin1: str(p.admin1), admin2: str(p.admin2), country: str(p.country), lat, lon };
}

function parseUrls(v: unknown): GalleryUrls {
  const u = obj(v);
  const out: GalleryUrls = {};
  for (const key of ["thumb", "large", "grid", "preview", "original", "download", "poster", "play"] as const) {
    const value = sameOriginPath(u[key]);
    if (value) out[key] = value;
  }
  return out;
}

export function parseItem(raw: unknown): GalleryItem | null {
  const r = obj(raw);
  const id = str(r.id);
  const kind = r.kind === "photo" || r.kind === "video" ? r.kind : null;
  if (!id || !kind) return null;
  const origin = obj(r.origin);
  const rotation = num(r.rotation);
  return {
    id,
    kind,
    title: str(r.title) ?? "",
    caption: str(r.caption),
    mime: str(r.mime) ?? "",
    size: num(r.size) ?? 0,
    takenAt: str(r.taken_at),
    addedAt: str(r.created_at) ?? str(r.added_at) ?? "",
    width: num(r.width),
    height: num(r.height),
    durationMs: num(r.duration_ms),
    place: parsePlace(r.place),
    camera: str(r.camera),
    source: str(r.source) ?? "",
    device: str(r.device),
    deviceName: str(r.device_name),
    origin: { filename: str(origin.filename), folder: str(origin.folder), importedFrom: str(origin.imported_from) },
    favorite: r.favorite === true,
    rotation: (rotation !== null && ROTATIONS.has(rotation) ? rotation : 0) as Rotation,
    albums: Array.isArray(r.albums) ? r.albums.filter((a): a is string => typeof a === "string") : [],
    trashedAt: str(r.trashed_at),
    trashExpiresAt: str(r.trash_expires_at),
    thumbhash: str(r.thumbhash),
    urls: parseUrls(r.urls),
  };
}

function parseSource(raw: unknown): GallerySource | null {
  const r = obj(raw);
  const id = str(r.id);
  if (!id) return null;
  return {
    id,
    label: str(r.label) ?? id,
    kind: (str(r.kind) ?? "other") as SourceKind,
    photos: num(r.photos) ?? 0,
    videos: num(r.videos) ?? 0,
    latestTakenAt: str(r.latest_taken_at),
  };
}

function parseExif(raw: unknown): Exif | null {
  if (!raw || typeof raw !== "object") return null;
  const e = obj(raw);
  return {
    make: str(e.make),
    model: str(e.model),
    lens: str(e.lens),
    iso: num(e.iso),
    exposureS: num(e.exposure_s),
    fNumber: num(e.f_number),
    focalMm: num(e.focal_mm),
    flash: typeof e.flash === "boolean" ? e.flash : null,
    orientation: num(e.orientation),
    software: str(e.software),
    takenAt: str(e.taken_at),
    offset: str(e.offset),
  };
}

// ---- requests ----------------------------------------------------------------------------------------------------

const enc = encodeURIComponent;

export function galleryQueryString(q: GalleryQuery): string {
  const params = new URLSearchParams();
  params.set("scope", "gallery");
  params.set("origin", q.origin ?? "all");
  params.set("kind", !q.kind || q.kind === "all" ? "photo,video" : q.kind);
  if (q.from) params.set("from", q.from);
  if (q.to) params.set("to", q.to);
  if (q.q?.trim()) params.set("q", q.q.trim());
  if (q.favorite) params.set("favorite", "true");
  if (q.trashed) params.set("trashed", "true");
  if (q.album) params.set("album", q.album);
  if (q.cursor) params.set("cursor", q.cursor);
  params.set("limit", String(Math.min(500, Math.max(1, q.limit ?? 200))));
  return params.toString();
}

export async function listGallery(q: GalleryQuery = {}): Promise<GalleryPage> {
  const body = obj(await request<unknown>("GET", `/v1/media/items?${galleryQueryString(q)}`));
  const items = (Array.isArray(body.items) ? body.items : []).map(parseItem).filter((i): i is GalleryItem => i !== null);
  return { items, nextCursor: str(body.next_cursor) };
}

export async function listSources(): Promise<{ sources: GallerySource[]; all: { photos: number; videos: number } }> {
  const body = obj(await request<unknown>("GET", "/v1/media/sources"));
  const all = obj(body.all);
  return {
    sources: (Array.isArray(body.sources) ? body.sources : []).map(parseSource).filter((s): s is GallerySource => s !== null),
    all: { photos: num(all.photos) ?? 0, videos: num(all.videos) ?? 0 },
  };
}

export async function listMonths(q: Pick<GalleryQuery, "origin" | "kind" | "trashed" | "favorite"> = {}): Promise<{ month: string; count: number }[]> {
  const params = new URLSearchParams({ origin: q.origin ?? "all", kind: !q.kind || q.kind === "all" ? "photo,video" : q.kind });
  if (q.trashed) params.set("trashed", "true");
  if (q.favorite) params.set("favorite", "true");
  const body = obj(await request<unknown>("GET", `/v1/media/items/months?${params}`));
  return (Array.isArray(body.months) ? body.months : [])
    .map((m) => ({ month: str(obj(m).month) ?? "", count: num(obj(m).count) ?? 0 }))
    .filter((m) => /^\d{4}-\d{2}$/.test(m.month));
}

export async function getDetails(id: string): Promise<ItemDetails | null> {
  const body = obj(await request<unknown>("GET", `/v1/media/items/${enc(id)}/details`));
  const item = parseItem(body.item);
  if (!item) return null;
  return { item, exif: parseExif(body.exif), people: Array.isArray(body.people) ? body.people.filter((p): p is string => typeof p === "string") : [] };
}

export interface ItemEdit {
  title?: string;
  caption?: string;
  favorite?: boolean;
  rotation?: Rotation;
  takenAt?: string;
}

export async function patchItem(id: string, edit: ItemEdit): Promise<GalleryItem | null> {
  const body: Raw = {};
  if (edit.title !== undefined) body.title = edit.title;
  if (edit.caption !== undefined) body.caption = edit.caption;
  if (edit.favorite !== undefined) body.favorite = edit.favorite;
  if (edit.rotation !== undefined) body.rotation = edit.rotation;
  if (edit.takenAt !== undefined) body.taken_at = edit.takenAt;
  return parseItem(await request<unknown>("PATCH", `/v1/media/items/${enc(id)}`, { body }));
}

const BATCH_MAX = 500;

/** One batch op over any number of items (500 per request). */
export async function batch(op: BatchOp, ids: string[], opts: { album?: string; alsoFromDevice?: boolean } = {}): Promise<BatchResult> {
  const out: BatchResult = { done: [], failed: [] };
  for (let i = 0; i < ids.length; i += BATCH_MAX) {
    const body: Raw = { op, ids: ids.slice(i, i + BATCH_MAX) };
    if (opts.album) body.album = opts.album;
    if (opts.alsoFromDevice) body.also_from_device = true;
    const r = obj(await request<unknown>("POST", "/v1/media/items/batch", { body, timeoutMs: 60_000 }));
    if (Array.isArray(r.done)) out.done.push(...r.done.filter((d): d is string => typeof d === "string"));
    if (Array.isArray(r.failed)) {
      for (const f of r.failed) {
        const id = str(obj(f).id);
        if (id) out.failed.push({ id, error: str(obj(f).error) ?? "failed" });
      }
    }
  }
  return out;
}

export async function listAlbums(): Promise<GalleryAlbum[]> {
  const body = obj(await request<unknown>("GET", "/v1/media/albums?scope=gallery"));
  return (Array.isArray(body.albums) ? body.albums : [])
    .map((a) => {
      const r = obj(a);
      const id = str(r.id);
      return id ? { id, title: str(r.title) ?? "", count: num(r.count) ?? num(r.items) ?? 0, system: typeof r.system_key === "string" } : null;
    })
    .filter((a): a is GalleryAlbum => a !== null);
}

export async function createAlbum(title: string): Promise<GalleryAlbum> {
  const r = obj(await request<unknown>("POST", "/v1/media/albums", { body: { title } }));
  const id = str(r.id);
  if (!id) throw new Error("album not created");
  return { id, title: str(r.title) ?? title, count: 0, system: false };
}

/** The tile image: the JPEG XL grid rendition where the browser decodes it, else the 256 px JPEG. */
export function tileUrl(item: GalleryItem, jxl: boolean): string | null {
  return (jxl ? item.urls.grid : undefined) ?? item.urls.thumb ?? null;
}

/** The viewer image: the 1440 px preview (JXL) where supported, else the 1024 px JPEG. */
export function previewUrl(item: GalleryItem, jxl: boolean): string | null {
  return (jxl ? item.urls.preview : undefined) ?? item.urls.large ?? item.urls.thumb ?? null;
}

// ---- share & compare ----------------------------------------------------------------------------------------------
// Links (POST shares for chosen photos, POST albums/{id}/shares for an album) and every stored form of one item.

export type ShareKind = "album" | "selection";

export interface GalleryShare {
  token: string;
  kind: ShareKind;
  /** The album, for an album link; null for a link over chosen photos (its hidden album is not a client's business). */
  albumId: string | null;
  title: string;
  /** Items the link serves now (trashed ones are not counted). */
  count: number;
  coverItem: string | null;
  coverThumb: string | null;
  createdAt: string;
  expiresAt: string | null;
  revokedAt: string | null;
  allowOriginal: boolean;
  views: number;
  lastViewAt: string | null;
  /** Not revoked and not expired. */
  live: boolean;
  /** `/a/<token>`: the public page, on the Secretary host. */
  publicPath: string;
}

export interface ShareOptions {
  /** 1, 7, 30; null = never expires. */
  expiresInDays: number | null;
  allowOriginal: boolean;
  title?: string;
}

export function parseShare(raw: unknown): GalleryShare | null {
  const r = obj(raw);
  const token = str(r.token);
  if (!token) return null;
  return {
    token,
    kind: r.kind === "selection" ? "selection" : "album",
    albumId: str(r.album_id),
    title: str(r.title) ?? "",
    count: num(r.count) ?? 0,
    coverItem: str(r.cover_item),
    coverThumb: sameOriginPath(r.cover_thumb) ?? null,
    createdAt: str(r.created_at) ?? "",
    expiresAt: str(r.expires_at),
    revokedAt: str(r.revoked_at),
    allowOriginal: r.allow_original === true,
    views: num(r.views) ?? 0,
    lastViewAt: str(r.last_view_at),
    live: r.live === true,
    publicPath: str(r.public_path) ?? `/a/${token}`,
  };
}

function shareBody(opts: ShareOptions): Raw {
  const body: Raw = { allow_original: opts.allowOriginal };
  if (opts.expiresInDays !== null) body.expires_in_days = opts.expiresInDays;
  return body;
}

/** A link over chosen photos and videos (up to 500). */
export async function createSelectionShare(itemIds: string[], opts: ShareOptions): Promise<GalleryShare> {
  const body = shareBody(opts);
  body.item_ids = itemIds;
  if (opts.title?.trim()) body.title = opts.title.trim();
  const share = parseShare(await request<unknown>("POST", "/v1/media/shares", { body }));
  if (!share) throw new Error("share not created");
  return share;
}

export async function createAlbumShare(albumId: string, opts: ShareOptions): Promise<GalleryShare> {
  const share = parseShare(await request<unknown>("POST", `/v1/media/albums/${enc(albumId)}/shares`, { body: shareBody(opts) }));
  if (!share) throw new Error("share not created");
  return share;
}

/** Every link, newest first, both kinds. */
export async function listShares(): Promise<GalleryShare[]> {
  const body = obj(await request<unknown>("GET", "/v1/media/shares"));
  return (Array.isArray(body.shares) ? body.shares : []).map(parseShare).filter((s): s is GalleryShare => s !== null);
}

/** Turns a link off for good (the page answers "gone"). */
export async function revokeShare(token: string): Promise<void> {
  await request<unknown>("DELETE", `/v1/media/shares/${enc(token)}`);
}

export type RenditionName = "grid" | "preview" | "thumb" | "large" | "poster" | "device_preview";

export interface StoredFile {
  format: string;
  mime: string;
  bytes: number | null;
  blake3: string | null;
  width: number | null;
  height: number | null;
  url: string;
}

export interface OriginalFile extends StoredFile {
  version: number;
  /** `store`, `spool` or `external`: where the server keeps it (never a path). */
  stored: string | null;
  reachable: boolean;
}

export interface OlderVersion extends StoredFile {
  version: number;
  replacedAt: string | null;
  stored: string | null;
  reachable: boolean;
}

export interface Rendition extends StoredFile {
  name: RenditionName;
}

/** What the device told the server when it uploaded (one entry per upload: an edit adds one). */
export interface DeviceUpload {
  filename: string | null;
  mime: string | null;
  uti: string | null;
  bytes: number | null;
  blake3: string | null;
  width: number | null;
  height: number | null;
  takenAt: string | null;
  uploadedAt: string | null;
  /** The stored version whose checksum equals the one the device reported; null when none (or none reported). */
  matchesVersion: number | null;
}

export interface DeviceCopy {
  id: string;
  name: string | null;
  kind: string | null;
  lastSeenAt: string | null;
  assetId: string | null;
  uploads: DeviceUpload[];
  latest: DeviceUpload | null;
}

export interface ItemRenditions {
  itemId: string;
  kind: string;
  original: OriginalFile;
  versions: OlderVersion[];
  renditions: Rendition[];
  device: DeviceCopy | null;
}

const RENDITION_NAMES = new Set<string>(["grid", "preview", "thumb", "large", "poster", "device_preview"]);

function parseStored(raw: unknown): StoredFile | null {
  const r = obj(raw);
  const url = sameOriginPath(r.url);
  if (!url) return null;
  return {
    format: str(r.format) ?? "",
    mime: str(r.mime) ?? "",
    bytes: num(r.bytes),
    blake3: str(r.blake3),
    width: num(r.width),
    height: num(r.height),
    url,
  };
}

function parseUpload(raw: unknown): DeviceUpload {
  const r = obj(raw);
  return {
    filename: str(r.filename),
    mime: str(r.mime),
    uti: str(r.uti),
    bytes: num(r.bytes),
    blake3: str(r.blake3),
    width: num(r.width),
    height: num(r.height),
    takenAt: str(r.taken_at),
    uploadedAt: str(r.uploaded_at),
    matchesVersion: num(r.matches_version),
  };
}

function parseDevice(raw: unknown): DeviceCopy | null {
  const r = obj(raw);
  const id = str(r.id);
  if (!id) return null;
  const uploads = (Array.isArray(r.uploads) ? r.uploads : []).map(parseUpload);
  return {
    id,
    name: str(r.name),
    kind: str(r.kind),
    lastSeenAt: str(r.last_seen_at),
    assetId: str(r.asset_id),
    uploads,
    latest: uploads.length ? uploads[uploads.length - 1]! : null,
  };
}

export function parseRenditions(raw: unknown): ItemRenditions | null {
  const r = obj(raw);
  const itemId = str(r.item_id);
  const original = parseStored(r.original);
  if (!itemId || !original) return null;
  const o = obj(r.original);
  return {
    itemId,
    kind: str(r.kind) ?? "photo",
    original: { ...original, version: num(o.version) ?? 1, stored: str(o.stored), reachable: o.reachable !== false },
    versions: (Array.isArray(r.versions) ? r.versions : [])
      .map((v): OlderVersion | null => {
        const file = parseStored(v);
        if (!file) return null;
        const x = obj(v);
        return { ...file, version: num(x.version) ?? 0, replacedAt: str(x.replaced_at), stored: str(x.stored), reachable: x.reachable !== false };
      })
      .filter((v): v is OlderVersion => v !== null),
    renditions: (Array.isArray(r.renditions) ? r.renditions : [])
      .map((v): Rendition | null => {
        const file = parseStored(v);
        const name = str(obj(v).name);
        return file && name && RENDITION_NAMES.has(name) ? { ...file, name: name as RenditionName } : null;
      })
      .filter((v): v is Rendition => v !== null),
    device: parseDevice(r.device),
  };
}

/** Every stored form of one item: original, older versions, display renditions and what the device reported. */
export async function getRenditions(id: string): Promise<ItemRenditions | null> {
  return parseRenditions(await request<unknown>("GET", `/v1/media/items/${enc(id)}/renditions`));
}
