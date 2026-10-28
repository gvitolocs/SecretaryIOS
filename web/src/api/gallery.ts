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
  /** Only the archived (hidden from the main Gallery); otherwise they are left out. */
  archived?: boolean;
  /** `west,south,east,north` in degrees: only pictures with a position inside it (a Places cluster). */
  bbox?: string;
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
  if (q.archived) params.set("archived", "true");
  if (q.bbox) params.set("bbox", q.bbox);
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
