// The Gallery's client for lucina-media (docs/media-library.md, API): the phones', the Mac's, the old backups' and the
// chat's photos and videos, per source, with their details, edits and the trash. Drive files are not here.
//
// Deleting in Lucina never deletes the original on the device; only `alsoFromDevice` asks the device to (it then asks
// the system, which confirms on the device).

import { parseEditList, type EditList } from "../lib/editList";
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
  /** The editor's full-resolution derivative; present only while the photo is edited (`urls.original` stays untouched). */
  edited?: string;
  downloadEdited?: string;
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
  /** The photo has editor changes: the renditions show them, `urls.edited` is the derivative, `urls.original` the file as taken. */
  edited: boolean;
  albums: string[];
  trashedAt: string | null;
  trashExpiresAt: string | null;
  thumbhash: string | null;
  /** A Live Photo: the id of the other half (the hidden motion clip), else null. Listings return only the still. */
  livePair: string | null;
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
  /** Only the archived (hidden from the main Gallery); otherwise they are left out. */
  archived?: boolean;
  /** `west,south,east,north` in degrees: only pictures with a position inside it (a Places cluster). */
  bbox?: string;
  album?: string;
  /** Only pictures with a face of this person (the people API: `people/{id}/items`, and smart search's `person=`). */
  person?: string;
  /** With `q`: rank by meaning (smart search) instead of matching words; see `listGalleryPage`. */
  smart?: boolean;
  /** Pictures that look like this one (smart search); `q` is ignored. */
  similarTo?: string;
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

/** A person as the photo's details name them (see `Person` for the people page's full record). */
export interface PersonRef {
  /** null for a name that comes as plain text (a server without the people API). */
  id: string | null;
  name: string | null;
}

export interface ItemDetails {
  item: GalleryItem;
  exif: Exif | null;
  people: PersonRef[];
}

export type BatchOp = "trash" | "restore" | "purge" | "favorite" | "unfavorite" | "album_add" | "album_remove" | "archive" | "unarchive";

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
  for (const key of ["thumb", "large", "grid", "preview", "original", "download", "poster", "play", "edited"] as const) {
    const value = sameOriginPath(u[key]);
    if (value) out[key] = value;
  }
  const downloadEdited = sameOriginPath(u.download_edited);
  if (downloadEdited) out.downloadEdited = downloadEdited;
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
    edited: r.edited === true,
    albums: Array.isArray(r.albums) ? r.albums.filter((a): a is string => typeof a === "string") : [],
    trashedAt: str(r.trashed_at),
    trashExpiresAt: str(r.trash_expires_at),
    thumbhash: str(r.thumbhash),
    livePair: str(r.live_pair),
    urls: parseUrls(r.urls),
  };
}

/** `[{id, name}]`; a plain string is a name with no person to open. */
export function parsePersonRefs(raw: unknown): PersonRef[] {
  if (!Array.isArray(raw)) return [];
  const out: PersonRef[] = [];
  for (const entry of raw) {
    if (typeof entry === "string") {
      if (entry) out.push({ id: null, name: entry });
      continue;
    }
    const r = obj(entry);
    const id = str(r.id);
    if (id) out.push({ id, name: str(r.name) });
  }
  return out;
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
  if (q.archived) params.set("archived", "true");
  if (q.bbox) params.set("bbox", q.bbox);
  if (q.album) params.set("album", q.album);
  if (q.person) params.set("person", q.person);
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

export async function listMonths(q: Pick<GalleryQuery, "origin" | "kind" | "trashed" | "favorite" | "archived"> = {}): Promise<{ month: string; count: number }[]> {
  const params = new URLSearchParams({ origin: q.origin ?? "all", kind: !q.kind || q.kind === "all" ? "photo,video" : q.kind });
  if (q.trashed) params.set("trashed", "true");
  if (q.favorite) params.set("favorite", "true");
  if (q.archived) params.set("archived", "true");
  const body = obj(await request<unknown>("GET", `/v1/media/items/months?${params}`));
  return (Array.isArray(body.months) ? body.months : [])
    .map((m) => ({ month: str(obj(m).month) ?? "", count: num(obj(m).count) ?? 0 }))
    .filter((m) => /^\d{4}-\d{2}$/.test(m.month));
}

export async function getDetails(id: string): Promise<ItemDetails | null> {
  const body = obj(await request<unknown>("GET", `/v1/media/items/${enc(id)}/details`));
  const item = parseItem(body.item);
  if (!item) return null;
  return { item, exif: parseExif(body.exif), people: parsePersonRefs(body.people) };
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
  // The album cards include the empty ones, so an album made on the albums page can be filled from the picker at once.
  return (await listAlbumCards())
    .filter((a) => !a.systemKey?.startsWith("share:"))
    .map((a) => ({ id: a.id, title: a.title, count: a.count, system: a.systemKey !== null }));
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

// ---- editor (components/gallery/editor): the edit list per item and the revert -----------------------------------------
// A save renders the derivative from the original on the server (a second or two for a phone photo), then answers with the
// item as it is now: `edited`, new rendition URLs, the derivative's size. The original file is never changed.

export interface ItemEdits {
  edited: boolean;
  /** The stored list (null when never edited). */
  edits: EditList | null;
  updatedAt: string | null;
  /** The unedited picture at 1600 px: what the editor works on, whatever the item's renditions show now. */
  baseUrl: string | null;
  item: GalleryItem | null;
}

function parseItemEdits(raw: unknown): ItemEdits {
  const body = obj(raw);
  return {
    edited: body.edited === true,
    edits: parseEditList(body.edits),
    updatedAt: str(body.updated_at),
    baseUrl: sameOriginPath(body.base_url) ?? null,
    item: parseItem(body.item),
  };
}

const EDIT_TIMEOUT_MS = 120_000;

export async function getEdits(id: string): Promise<ItemEdits> {
  return parseItemEdits(await request<unknown>("GET", `/v1/media/items/${enc(id)}/edits`));
}

/** Save `list` (an identity list reverts) and return the item as the server has it now. */
export async function putEdits(id: string, list: EditList): Promise<ItemEdits> {
  return parseItemEdits(await request<unknown>("PUT", `/v1/media/items/${enc(id)}/edits`, { body: list, timeoutMs: EDIT_TIMEOUT_MS }));
}

/** Revert to the original: the edit list and the derivative go, the renditions come from the original again. */
export async function deleteEdits(id: string): Promise<ItemEdits> {
  return parseItemEdits(await request<unknown>("DELETE", `/v1/media/items/${enc(id)}/edits`, { timeoutMs: EDIT_TIMEOUT_MS }));
}

// ---- browse (places, memories, archive) ----------------------------------------------------------------------------

/** `west, south, east, north` in degrees. */
export type BBox = [number, number, number, number];

/** Pictures near each other on the map: how many, their centre, the box around them and the one that stands for them. */
export interface PlaceCluster {
  id: string;
  count: number;
  lat: number;
  lon: number;
  bbox: BBox;
  cover: GalleryItem;
}

export interface PlacesResult {
  zoom: number;
  /** All pictures with a position under the filters, in the whole `bbox`. */
  total: number;
  clusters: PlaceCluster[];
}

/** One past year of "On this day": a few of the best pictures, in time order, and how many there were that day. */
export interface MemoryYear {
  year: number;
  yearsAgo: number;
  total: number;
  items: GalleryItem[];
}

const parseItems = (v: unknown): GalleryItem[] => (Array.isArray(v) ? v : []).map(parseItem).filter((i): i is GalleryItem => i !== null);

function parseBBox(v: unknown): BBox | null {
  if (!Array.isArray(v) || v.length !== 4) return null;
  const n = v.map(num);
  return n.every((x) => x !== null) ? (n as BBox) : null;
}

/** Map clusters for the visible window (`bbox`) at a map zoom; sources and kinds as in the grid. */
export async function listPlaces(q: { origin?: string; kind?: GalleryKind | "all"; bbox?: BBox; zoom?: number; limit?: number } = {}): Promise<PlacesResult> {
  const params = new URLSearchParams({ origin: q.origin ?? "all", kind: !q.kind || q.kind === "all" ? "photo,video" : q.kind });
  if (q.bbox) params.set("bbox", q.bbox.join(","));
  if (q.zoom !== undefined) params.set("zoom", String(q.zoom));
  if (q.limit) params.set("limit", String(q.limit));
  const body = obj(await request<unknown>("GET", `/v1/media/items/places?${params}`));
  const clusters = (Array.isArray(body.clusters) ? body.clusters : [])
    .map((raw): PlaceCluster | null => {
      const c = obj(raw);
      const id = str(c.id);
      const cover = parseItem(c.cover);
      const lat = num(c.lat);
      const lon = num(c.lon);
      const bbox = parseBBox(c.bbox);
      return id && cover && lat !== null && lon !== null && bbox ? { id, count: num(c.count) ?? 0, lat, lon, bbox, cover } : null;
    })
    .filter((c): c is PlaceCluster => c !== null);
  return { zoom: num(body.zoom) ?? q.zoom ?? 0, total: num(body.total) ?? 0, clusters };
}

/** The pictures taken on this calendar day (`MM-DD`, today by default) in previous years, newest year first. */
export async function listOnThisDay(q: { date?: string; origin?: string; perYear?: number } = {}): Promise<MemoryYear[]> {
  const params = new URLSearchParams({ origin: q.origin ?? "all" });
  if (q.date) params.set("date", q.date);
  if (q.perYear) params.set("per_year", String(q.perYear));
  const body = obj(await request<unknown>("GET", `/v1/media/items/on-this-day?${params}`));
  return (Array.isArray(body.years) ? body.years : [])
    .map((raw): MemoryYear | null => {
      const y = obj(raw);
      const year = num(y.year);
      const list = parseItems(y.items);
      return year !== null && list.length ? { year, yearsAgo: num(y.years_ago) ?? 0, total: num(y.total) ?? list.length, items: list } : null;
    })
    .filter((y): y is MemoryYear => y !== null);
}

/** The newest arrivals (by when they were added to the library). */
export async function listRecent(q: { origin?: string; days?: number; limit?: number } = {}): Promise<{ items: GalleryItem[]; total: number }> {
  const params = new URLSearchParams({ origin: q.origin ?? "all" });
  if (q.days) params.set("days", String(q.days));
  params.set("limit", String(q.limit ?? 30));
  const body = obj(await request<unknown>("GET", `/v1/media/items/recent?${params}`));
  return { items: parseItems(body.items), total: num(body.total) ?? 0 };
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

// ---- lab (AI Lab: "Edit with Lucina…") ------------------------------------------------------------------------------
// A photo, a prompt and an optional reference photo go to a local image model that runs one job at a time. The result
// waits in the Lab until it is accepted (a new library item linked to the original; the original is never changed) or
// discarded. Lucina notifies the owner when a job finishes.

export type LabStatus = "queued" | "running" | "done" | "failed" | "accepted" | "discarded";
export type LabPhase = "waiting" | "generating";

export interface LabOriginal {
  id: string;
  title: string;
  width: number | null;
  height: number | null;
  rotation: Rotation;
  thumbhash: string | null;
  trashed: boolean;
}

export interface LabJob {
  id: string;
  itemId: string;
  referenceItemId: string | null;
  prompt: string;
  status: LabStatus;
  /** While running: `waiting` for the GPU, then `generating`. */
  phase: LabPhase | null;
  /** 0 to 100. */
  progress: number;
  error: string | null;
  seed: number | null;
  steps: number | null;
  model: string | null;
  /** 1 for the job that runs next; null when the job is not queued. */
  queuePosition: number | null;
  createdAt: string;
  startedAt: string | null;
  finishedAt: string | null;
  acceptedItemId: string | null;
  width: number | null;
  height: number | null;
  original: LabOriginal | null;
  urls: { before: string | null; after: string | null; reference: string | null };
}

const LAB_STATUSES = new Set(["queued", "running", "done", "failed", "accepted", "discarded"]);

export function parseLabJob(raw: unknown): LabJob | null {
  const r = obj(raw);
  const id = str(r.id);
  const itemId = str(r.item_id);
  const status = str(r.status);
  if (!id || !itemId || !status || !LAB_STATUSES.has(status)) return null;
  const urls = obj(r.urls);
  const result = obj(r.result);
  const original = r.original && typeof r.original === "object" ? obj(r.original) : null;
  const rotation = num(original?.rotation);
  const phase = str(r.phase);
  return {
    id,
    itemId,
    referenceItemId: str(r.reference_item_id),
    prompt: str(r.prompt) ?? "",
    status: status as LabStatus,
    phase: phase === "waiting" || phase === "generating" ? phase : null,
    progress: Math.min(100, Math.max(0, Math.round(num(r.progress) ?? 0))),
    error: str(r.error),
    seed: num(r.seed),
    steps: num(r.steps),
    model: str(r.model),
    queuePosition: num(r.queue_position),
    createdAt: str(r.created_at) ?? "",
    startedAt: str(r.started_at),
    finishedAt: str(r.finished_at),
    acceptedItemId: str(r.accepted_item_id),
    width: num(result.width) ?? num(original?.width),
    height: num(result.height) ?? num(original?.height),
    original: original
      ? {
          id: str(original.id) ?? itemId,
          title: str(original.title) ?? "",
          width: num(original.width),
          height: num(original.height),
          rotation: (rotation !== null && ROTATIONS.has(rotation) ? rotation : 0) as Rotation,
          thumbhash: str(original.thumbhash),
          trashed: original.trashed === true,
        }
      : null,
    urls: { before: sameOriginPath(urls.before) ?? null, after: sameOriginPath(urls.after) ?? null, reference: sameOriginPath(urls.reference) ?? null },
  };
}

/** The Lab's jobs (discarded ones are left out unless asked for), and whether a model is connected to run them. */
export async function listLab(opts: { status?: LabStatus[]; itemId?: string } = {}): Promise<{ enabled: boolean; jobs: LabJob[] }> {
  const params = new URLSearchParams();
  if (opts.status?.length) params.set("status", opts.status.join(","));
  if (opts.itemId) params.set("item_id", opts.itemId);
  const query = params.toString();
  const body = obj(await request<unknown>("GET", `/v1/media/lab${query ? `?${query}` : ""}`));
  const jobs = (Array.isArray(body.jobs) ? body.jobs : []).map(parseLabJob).filter((j): j is LabJob => j !== null);
  return { enabled: body.enabled !== false, jobs };
}

export async function getLabJob(id: string): Promise<LabJob | null> {
  return parseLabJob(await request<unknown>("GET", `/v1/media/lab/${enc(id)}`));
}

export const LAB_PROMPT_MAX = 1000;

/** Queue an edit of `itemId`. Resolves the job (status queued). */
export async function createLabJob(itemId: string, edit: { prompt: string; referenceItemId?: string | null }): Promise<LabJob> {
  const body: Raw = { prompt: edit.prompt.trim() };
  if (edit.referenceItemId) body.reference_item_id = edit.referenceItemId;
  const job = parseLabJob(await request<unknown>("POST", `/v1/media/items/${enc(itemId)}/lab`, { body }));
  if (!job) throw new Error("job not created");
  return job;
}

export interface AcceptedEdit {
  job: LabJob | null;
  /** The new library item (null when the server's answer had none). */
  item: GalleryItem | null;
  /** The result was already in the library: no new item was made. */
  duplicate: boolean;
}

/** The result becomes a new library item linked to the original. */
export async function acceptLabJob(id: string): Promise<AcceptedEdit> {
  const body = obj(await request<unknown>("POST", `/v1/media/lab/${enc(id)}/accept`, { body: {} }));
  return { job: parseLabJob(body.job), item: parseItem(body.item), duplicate: body.duplicate === true };
}

/** Delete the result. */
export async function discardLabJob(id: string): Promise<LabJob | null> {
  return parseLabJob(await request<unknown>("POST", `/v1/media/lab/${enc(id)}/discard`, { body: {} }));
}

/** Queue a failed job again. */
export async function retryLabJob(id: string): Promise<LabJob | null> {
  return parseLabJob(await request<unknown>("POST", `/v1/media/lab/${enc(id)}/retry`, { body: {} }));
}

/** Cancel a queued job, or remove a failed one. */
export async function cancelLabJob(id: string): Promise<void> {
  await request<unknown>("DELETE", `/v1/media/lab/${enc(id)}`);
}

// ---- albums page (views/GalleryAlbums.tsx, components/gallery/albums) ---------------------------------------------------
// An album as the server lists it, with the cover and the dates. Albums made for a photo-selection share (`share:` keys)
// are not the owner's and are never shown; the other built-in albums (Lucina's) have a `systemKey` and can't be deleted.

export interface AlbumCard {
  id: string;
  title: string;
  /** Set for the built-in albums. */
  systemKey: string | null;
  coverItem: string | null;
  count: number;
  createdAt: string;
  updatedAt: string;
}

export function parseAlbumCard(raw: unknown): AlbumCard | null {
  const r = obj(raw);
  const id = str(r.id);
  if (!id) return null;
  return {
    id,
    title: str(r.title) ?? "",
    systemKey: str(r.system_key),
    coverItem: str(r.cover_item),
    count: num(r.count) ?? 0,
    createdAt: str(r.created_at) ?? "",
    updatedAt: str(r.updated_at) ?? "",
  };
}

const parseAlbumCards = (v: unknown): AlbumCard[] => (Array.isArray(v) ? v : []).map(parseAlbumCard).filter((a): a is AlbumCard => a !== null);

/**
 * Every album with something in the Gallery, plus the empty ones (a new album has no photos yet, and the Gallery scope
 * leaves out albums with nothing in it). Albums whose photos are all elsewhere (film folders, Drive) are not listed.
 */
export async function listAlbumCards(): Promise<AlbumCard[]> {
  const [inGallery, everywhere] = await Promise.all([
    request<unknown>("GET", "/v1/media/albums?scope=gallery"),
    request<unknown>("GET", "/v1/media/albums").catch(() => null),
  ]);
  const shown = parseAlbumCards(obj(inGallery).albums);
  const seen = new Set(shown.map((a) => a.id));
  const empty = parseAlbumCards(obj(everywhere).albums).filter((a) => a.count === 0 && !seen.has(a.id));
  return [...shown, ...empty];
}

export async function getAlbum(id: string): Promise<AlbumCard | null> {
  return parseAlbumCard(await request<unknown>("GET", `/v1/media/albums/${enc(id)}`));
}

export async function renameAlbum(id: string, title: string): Promise<AlbumCard> {
  const album = parseAlbumCard(await request<unknown>("PATCH", `/v1/media/albums/${enc(id)}`, { body: { title } }));
  if (!album) throw new Error("album not renamed");
  return album;
}

/** Deletes the album only; its photos stay in the Gallery. */
export async function deleteAlbum(id: string): Promise<void> {
  await request<unknown>("DELETE", `/v1/media/albums/${enc(id)}`);
}

/** One item by id (an album's cover). */
export async function getItem(id: string): Promise<GalleryItem | null> {
  return parseItem(await request<unknown>("GET", `/v1/media/items/${enc(id)}`));
}

// ---- smart search (components/gallery/smart) -----------------------------------------------------------------------------
// Pictures ranked by meaning: the server embeds every photo (CLIP-style) and answers a text query or "like this one" with
// the closest first, not by date. It can be switched off (503), or have looked at only part of the library so far.

export interface EmbeddingStatus {
  enabled: boolean;
  model: string | null;
  /** Photos the index has looked at. */
  indexed: number;
  /** Photos still waiting to be looked at. */
  pending: number;
  running: boolean;
  /** Paused because the machine is hot. */
  pausedHot: boolean;
}

export function parseEmbeddingStatus(raw: unknown): EmbeddingStatus | null {
  if (!raw || typeof raw !== "object") return null;
  const r = obj(raw);
  return {
    enabled: r.enabled === true,
    model: str(r.model),
    indexed: Math.max(0, num(r.indexed) ?? 0),
    pending: Math.max(0, num(r.pending) ?? 0),
    running: r.running === true,
    pausedHot: r.paused_hot === true,
  };
}

/** Smart search is off on this server (503), or the server has no such route. */
export class SmartUnavailableError extends Error {
  constructor() {
    super("smart search is not available");
    this.name = "SmartUnavailableError";
  }
}

export interface SmartPage extends GalleryPage {
  /** 0 to 1, higher is closer, in the order of `items`. */
  scores: number[];
  status: EmbeddingStatus | null;
}

/** null when the server has no smart search or it is off. */
export async function getEmbeddingStatus(): Promise<EmbeddingStatus | null> {
  try {
    return parseEmbeddingStatus(await request<unknown>("GET", "/v1/media/embeddings/status"));
  } catch {
    return null;
  }
}

export const SMART_PAGE_MAX = 200;

async function smartRequest(path: string, q: GalleryQuery): Promise<SmartPage> {
  const limit = Math.min(SMART_PAGE_MAX, Math.max(1, q.limit ?? 60));
  let body: Raw;
  try {
    body = obj(await request<unknown>("GET", `${path}?${galleryQueryString({ ...q, limit })}`));
  } catch (e) {
    const status = (e as { status?: unknown }).status;
    if (status === 503 || status === 404 || status === 501) throw new SmartUnavailableError();
    throw e;
  }
  const items = (Array.isArray(body.items) ? body.items : []).map(parseItem).filter((i): i is GalleryItem => i !== null);
  const scores = Array.isArray(body.scores) ? body.scores.map((s) => num(s) ?? 0) : [];
  return { items, scores, nextCursor: str(body.next_cursor), status: parseEmbeddingStatus(body.status) };
}

/** Photos ranked by how well they match the words in `q.q`; the other fields filter as in the plain listing. */
export function searchSmart(q: GalleryQuery): Promise<SmartPage> {
  return smartRequest("/v1/media/items/search", q);
}

/** Photos that look like `id` (the picture itself is left out). */
export function listSimilar(id: string, q: GalleryQuery = {}): Promise<SmartPage> {
  return smartRequest(`/v1/media/items/${enc(id)}/similar`, { ...q, q: undefined });
}

/** A page of the Gallery for a query: similar pictures, a smart search, or the plain (words) listing. */
export async function listGalleryPage(q: GalleryQuery): Promise<GalleryPage> {
  if (q.similarTo) return listSimilar(q.similarTo, q);
  if (q.smart && q.q?.trim()) return searchSmart(q);
  return listGallery(q);
}

// ---- people (views/GalleryPeople.tsx, GalleryPerson via Gallery, components/gallery/people) --------------------------------
// Face recognition runs on this server only and is off until the owner turns it on. Faces are found once per photo, grouped
// into people, and a face the owner assigns by hand is never moved by the grouping. Boxes are fractions (0..1) of the picture
// as the renditions show it, before the rotation the client draws.

export interface FaceSettings {
  enabled: boolean;
  model: string | null;
  /** Photos looked at / still waiting. */
  scanned: number;
  pending: number;
  faces: number;
  people: number;
  running: boolean;
  pausedHot: boolean;
  pausedBusy: boolean;
}

export interface Person {
  id: string;
  /** null until the owner names the person. */
  name: string | null;
  hidden: boolean;
  /** `YYYY-MM-DD`, else null. */
  birthDate: string | null;
  faceCount: number;
  /** Photos with a face of this person. */
  itemCount: number;
  coverFaceId: string | null;
  thumbUrl: string | null;
}

export interface FaceBox {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface ItemFace {
  id: string;
  box: FaceBox;
  score: number;
  /** `manual`: the owner assigned it, the grouping leaves it alone. */
  source: "ml" | "manual";
  person: { id: string; name: string | null } | null;
}

export interface UnassignedFace {
  id: string;
  itemId: string | null;
  thumbUrl: string;
  score: number;
}

/** Where a face goes: a person, nobody (`personId: null`), or a new person with this name. */
export type FaceTarget = { personId: string | null } | { newPerson: string };

export const PERSON_NAME_MAX = 80;
export const UNASSIGNED_PAGE = 60;

export const faceThumbUrl = (faceId: string): string => `/v1/media/faces/${enc(faceId)}/thumb`;

export function parseFaceSettings(raw: unknown): FaceSettings {
  const r = obj(raw);
  const count = (v: unknown) => Math.max(0, Math.round(num(v) ?? 0));
  return {
    enabled: r.enabled === true,
    model: str(r.model),
    scanned: count(r.scanned),
    pending: count(r.pending),
    faces: count(r.faces),
    people: count(r.people),
    running: r.running === true,
    pausedHot: r.paused_hot === true,
    pausedBusy: r.paused_busy === true,
  };
}

const date = (v: unknown): string | null => (typeof v === "string" && /^\d{4}-\d{2}-\d{2}$/.test(v) ? v : null);

export function parsePerson(raw: unknown): Person | null {
  const r = obj(raw);
  const id = str(r.id);
  if (!id) return null;
  return {
    id,
    name: str(r.name),
    hidden: r.hidden === true,
    birthDate: date(r.birth_date),
    faceCount: Math.max(0, Math.round(num(r.face_count) ?? 0)),
    itemCount: Math.max(0, Math.round(num(r.item_count) ?? 0)),
    coverFaceId: str(r.cover_face_id),
    thumbUrl: sameOriginPath(r.thumb_url) ?? null,
  };
}

/** A box inside the picture with a size; anything else is dropped (the face is not drawn). */
export function parseFaceBox(raw: unknown): FaceBox | null {
  const b = obj(raw);
  const x = num(b.x);
  const y = num(b.y);
  const w = num(b.w);
  const h = num(b.h);
  if (x === null || y === null || w === null || h === null || w <= 0 || h <= 0) return null;
  const left = Math.min(1, Math.max(0, x));
  const top = Math.min(1, Math.max(0, y));
  const width = Math.min(1 - left, w);
  const height = Math.min(1 - top, h);
  return width > 0 && height > 0 ? { x: left, y: top, w: width, h: height } : null;
}

export function parseItemFace(raw: unknown): ItemFace | null {
  const r = obj(raw);
  const id = str(r.id);
  const box = parseFaceBox(r.box);
  if (!id || !box) return null;
  const person = obj(r.person);
  const personId = str(person.id);
  return {
    id,
    box,
    score: num(r.score) ?? 0,
    source: r.source === "manual" ? "manual" : "ml",
    person: personId ? { id: personId, name: str(person.name) } : null,
  };
}

export async function getFaceSettings(): Promise<FaceSettings> {
  return parseFaceSettings(await request<unknown>("GET", "/v1/media/faces/settings"));
}

/** Turn recognition on or off. Off stops the scanning; nothing is deleted. */
export async function setFacesEnabled(enabled: boolean): Promise<FaceSettings> {
  return parseFaceSettings(await request<unknown>("PUT", "/v1/media/faces/settings", { body: { enabled } }));
}

/** Delete every face, person and crop (recognition is turned off too). */
export async function deleteFaceData(): Promise<void> {
  await request<unknown>("DELETE", "/v1/media/faces", { timeoutMs: 60_000 });
}

export async function listPeople(opts: { minFaces?: number; includeHidden?: boolean } = {}): Promise<Person[]> {
  const params = new URLSearchParams();
  if (opts.minFaces !== undefined) params.set("min_faces", String(opts.minFaces));
  if (opts.includeHidden) params.set("include_hidden", "true");
  const query = params.toString();
  const body = obj(await request<unknown>("GET", `/v1/media/people${query ? `?${query}` : ""}`));
  return (Array.isArray(body.people) ? body.people : []).map(parsePerson).filter((p): p is Person => p !== null);
}

export async function getPerson(id: string): Promise<Person | null> {
  return parsePerson(await request<unknown>("GET", `/v1/media/people/${enc(id)}`));
}

export interface PersonEdit {
  /** null (or blank) removes the name. */
  name?: string | null;
  hidden?: boolean;
  /** `YYYY-MM-DD`, or null to clear it. */
  birthDate?: string | null;
  coverFaceId?: string;
}

export async function patchPerson(id: string, edit: PersonEdit): Promise<Person> {
  const body: Raw = {};
  if (edit.name !== undefined) body.name = edit.name === null ? null : edit.name.trim().slice(0, PERSON_NAME_MAX) || null;
  if (edit.hidden !== undefined) body.hidden = edit.hidden;
  if (edit.birthDate !== undefined) body.birth_date = edit.birthDate;
  if (edit.coverFaceId !== undefined) body.cover_face_id = edit.coverFaceId;
  const person = parsePerson(await request<unknown>("PATCH", `/v1/media/people/${enc(id)}`, { body }));
  if (!person) throw new Error("person not updated");
  return person;
}

/** Move the faces of `from` to `keep` and delete those people. */
export async function mergePeople(keep: string, from: string[]): Promise<Person> {
  const person = parsePerson(await request<unknown>("POST", `/v1/media/people/${enc(keep)}/merge`, { body: { from }, timeoutMs: 60_000 }));
  if (!person) throw new Error("people not merged");
  return person;
}

/** A page of one person's photos: the listing's own filters and paging, the person's route. */
export async function listPersonItems(id: string, q: GalleryQuery = {}): Promise<GalleryPage> {
  const body = obj(await request<unknown>("GET", `/v1/media/people/${enc(id)}/items?${galleryQueryString({ ...q, person: undefined })}`));
  const items = (Array.isArray(body.items) ? body.items : []).map(parseItem).filter((i): i is GalleryItem => i !== null);
  return { items, nextCursor: str(body.next_cursor) };
}

export async function listItemFaces(itemId: string): Promise<ItemFace[]> {
  const body = obj(await request<unknown>("GET", `/v1/media/items/${enc(itemId)}/faces`));
  return (Array.isArray(body.faces) ? body.faces : []).map(parseItemFace).filter((f): f is ItemFace => f !== null);
}

/** Assign a face (to a person, to nobody, or to a new person). The server marks it as the owner's choice. */
export async function assignFace(faceId: string, target: FaceTarget): Promise<ItemFace | null> {
  const body: Raw = "newPerson" in target ? { new_person: target.newPerson.trim().slice(0, PERSON_NAME_MAX) } : { person_id: target.personId };
  return parseItemFace(await request<unknown>("PUT", `/v1/media/faces/${enc(faceId)}`, { body }));
}

export async function listUnassignedFaces(opts: { cursor?: string | null; limit?: number } = {}): Promise<{ faces: UnassignedFace[]; nextCursor: string | null }> {
  const params = new URLSearchParams({ limit: String(opts.limit ?? UNASSIGNED_PAGE) });
  if (opts.cursor) params.set("cursor", opts.cursor);
  const body = obj(await request<unknown>("GET", `/v1/media/faces/unassigned?${params}`));
  const faces: UnassignedFace[] = [];
  for (const entry of Array.isArray(body.faces) ? body.faces : []) {
    const r = obj(entry);
    const id = str(r.id);
    if (!id) continue;
    faces.push({ id, itemId: str(r.item_id), thumbUrl: sameOriginPath(r.thumb_url) ?? faceThumbUrl(id), score: num(r.score) ?? 0 });
  }
  return { faces, nextCursor: str(body.next_cursor) };
}
