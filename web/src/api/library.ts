import { request } from "./client";

export type ReadingStatus = "unknown" | "not_started" | "reading" | "completed" | "abandoned";
export type BookKind = "book" | "dictionary" | "document";
export type Drm = "none" | "protected" | "unknown";
export type Format = "azw3" | "azw" | "mobi" | "kfx" | "pdf" | "txt" | "md" | "prc" | "pobi" | "epub";
export interface Progress {
  percent: number | null;
  location: number | null;
  page: number | null;
  source: string;
  at: string;
}
export interface KindlePresence {
  path: string;
  sizeBytes: number;
  lastSeenAt: string;
}
export interface Brief {
  id: string;
  kind: "extractive" | "model";
  text: string;
  generatedAt: string;
  model: string | null;
  basedOn: string[];
}
export interface BookSummary {
  id: string;
  title: string;
  authors: string[];
  kind?: BookKind;
  status: ReadingStatus;
  progress: Progress | null;
  onKindle: KindlePresence | null;
  owned: boolean;
  formats: Format[];
  drm: Drm;
  highlightCount: number;
  lastHighlightAt: string | null;
  updatedAt: string;
}
export interface Book extends Omit<BookSummary, "highlightCount" | "lastHighlightAt"> {
  isbn: string | null;
  doi: string | null;
  asin: string | null;
  fingerprints: string[];
  sources: { kind: string; device: string | null }[];
  removedFromKindleAt: string | null;
  mentionedOnly: boolean;
  statusSource: string | null;
  lastOpenedAt: string | null;
  tags: string[];
  briefs: Brief[];
  relatedNotes: { vault: string; path: string; title: string; linkedAt: string; how: string }[];
  memoryRefs: { kind: string; id: string; at: string }[];
  createdAt: string;
}
export interface Location {
  start: number;
  end: number;
}
export interface Highlight {
  id: string;
  bookId: string;
  kind: "highlight" | "note" | "bookmark";
  text: string;
  note: string | null;
  location: Location | null;
  page: string | null;
  addedAt: string | null;
  addedAtRaw: string;
  source: string;
  provenance: { file: string; entryIndex: number; device: string | null };
  /** Opaque server identity; never interpreted by the UI. */
  dedupKey: string;
}
export interface ReadingSession {
  id: string;
  bookId: string;
  startedAt: string;
  endedAt: string | null;
  startLocation: number | null;
  endLocation: number | null;
  source: string;
}
export interface BookDetail {
  book: Book;
  highlights: Highlight[];
  sessions: ReadingSession[];
}
export interface SearchHit {
  bookId: string;
  title: string;
  authors: string[];
  highlightId: string | null;
  snippet: string;
  location: Location | null;
  page: string | null;
  source: string;
}
export interface Recent {
  highlights: Highlight[];
  changes: { bookId: string; status: ReadingStatus | null; progress: Progress | null; at: string; source: string }[];
  sessions: ReadingSession[];
  readingSessions: "unavailable_on_stock_kindle" | "available";
  unknownDateHighlightsExcluded: boolean;
  kindleDatesHaveNoTimezone: boolean;
}
export interface ImportOutcome {
  booksAdded: number;
  booksUpdated: number;
  booksUnchanged: number;
  highlightsAdded: number;
  highlightsSkipped: number;
  removedFromKindle: number;
  conflicts: string[];
  warnings: string[];
}
export interface ImportLog {
  time: string;
  caller: string;
  source: string;
  booksAdded: number;
  booksUpdated: number;
  booksUnchanged: number;
  highlightsAdded: number;
  highlightsSkipped: number;
  removedFromKindle: number;
  conflictCount: number;
  warningCount: number;
}
export interface Sync {
  imports: ImportLog[];
  books: number;
  highlights: number;
}
export interface Stats {
  byStatus: Partial<Record<ReadingStatus, number>>;
  byFormat: Partial<Record<Format, number>>;
  byDrm: Partial<Record<Drm, number>>;
  highlights: number;
  booksOnKindle: number;
  lastImport: ImportLog | null;
}
export interface ImportBatch {
  source: string;
  device: string | null;
  scannedAt: string;
  books: {
    path: string;
    sizeBytes: number;
    fingerprint: string;
    format: Format;
    sidecarPresent?: boolean;
    sidecarExtensions: string[];
    metadata: {
      title: string | null;
      authors: string[];
      publisher: string | null;
      isbn: string | null;
      asin: string | null;
      cdetype: string | null;
      language: string | null;
      drm: Drm;
    };
  }[];
  clippings: { title: string; authors: string[]; highlight: Highlight }[];
}
export interface BookFilters {
  q?: string;
  status?: ReadingStatus;
  onKindle?: boolean;
  kind?: string;
  limit?: number;
}
export type BookPatch = { status?: ReadingStatus; tags?: string[]; progress?: Pick<Progress, "percent" | "location" | "page"> };
const CLIENT = { "X-Secretary-Client": "web" };
const path = (id: string) => `/v1/library/books/${encodeURIComponent(id)}`;
function query(values: object): string {
  const p = new URLSearchParams();
  for (const [key, value] of Object.entries(values)) if (value !== undefined && value !== "") p.set(key, String(value));
  return p.size ? `?${p}` : "";
}
export const libraryApi = {
  list: async (filters: BookFilters = {}): Promise<BookSummary[]> =>
    (await request<{ books: BookSummary[] }>("GET", `/v1/library/books${query(filters)}`)).books,
  get: (id: string) => request<BookDetail>("GET", path(id)),
  update: (id: string, patch: BookPatch, baseUpdatedAt: string) =>
    request<Book>("PATCH", path(id), { body: { ...patch, baseUpdatedAt }, headers: CLIENT }),
  highlights: async (id: string, q = "", limit = 200): Promise<Highlight[]> =>
    (await request<{ highlights: Highlight[] }>("GET", `${path(id)}/highlights${query({ q, limit })}`)).highlights,
  search: async (q: string, limit = 200): Promise<SearchHit[]> =>
    (await request<{ results: SearchHit[] }>("GET", `/v1/library/search${query({ q, limit })}`)).results,
  recent: (days = 7, limit = 500) => request<Recent>("GET", `/v1/library/recent${query({ days, limit })}`),
  stats: () => request<Stats>("GET", "/v1/library/stats"),
  sync: () => request<Sync>("GET", "/v1/library/sync"),
  import: (batch: ImportBatch) => request<ImportOutcome>("POST", "/v1/library/import", { body: batch, headers: CLIENT }),
  brief: (id: string) => request<Brief>("POST", `${path(id)}/brief`, { headers: CLIENT }),
};
