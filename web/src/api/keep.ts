// Keep (docs: keep-contract.md): Google-Keep-style notes on the existing note store.
// Labels are the store's `tags`; GET /v1/notes/changes feeds the local cache (state/keep.ts).

import { ApiError, request } from "./client";

const CLIENT = { "X-Secretary-Client": "web" };

export const KEEP_COLORS = ["default", "red", "orange", "yellow", "green", "teal", "blue", "cerulean", "purple", "pink", "brown", "gray"] as const;
export type KeepColor = (typeof KEEP_COLORS)[number];
export const RECURRENCES = ["daily", "weekly", "monthly", "yearly"] as const;
export type Recurrence = (typeof RECURRENCES)[number];

export interface ChecklistItem {
  id: string;
  text: string;
  checked: boolean;
  indent: 0 | 1;
}
export interface Attachment {
  id: string;
  file: string;
  mimetype: string;
  size: number;
  name?: string;
  createdAt: string;
}
export interface Annotation {
  url: string;
  title?: string;
  description?: string;
  source?: string;
}
export interface Reminder {
  at: string;
  recurrence?: Recurrence;
  lucinaId?: string;
}

/** A full note as the server stores it (live, archived or in the trash). */
export interface KeepNote {
  id: string;
  slug: string;
  title: string;
  content: string;
  /** Normalised labels (the store's tags). Display names come from the label registry. */
  tags: string[];
  color: KeepColor;
  pinned: boolean;
  archived: boolean;
  /** Present (possibly empty) = a list note. */
  items?: ChecklistItem[];
  attachments: Attachment[];
  annotations: Annotation[];
  collaborators: string[];
  reminder?: Reminder;
  position: number;
  rev: number;
  source: string;
  createdAt: string;
  updatedAt: string;
  deletedAt?: string;
}

export interface LabelInfo {
  tag: string;
  name: string;
  count: number;
}

/** What the client may send in POST / PATCH (the contract's model fields; labels are display names). */
export interface NoteInput {
  title?: string;
  content?: string;
  color?: KeepColor;
  pinned?: boolean;
  archived?: boolean;
  items?: ChecklistItem[];
  labels?: string[];
  collaborators?: string[];
  reminder?: Reminder | null;
  position?: number;
  convert?: "text" | "list";
}

export interface BatchInput {
  ids: string[];
  set?: { color?: KeepColor; pinned?: boolean; archived?: boolean };
  addLabels?: string[];
  removeLabels?: string[];
  trash?: true;
  restore?: true;
  deleteForever?: true;
}

export interface Changes {
  notes: KeepNote[];
  removed: string[];
  cursor: string;
  hasMore: boolean;
}

export interface ImportReport {
  dryRun: boolean;
  files: { json: number; html: number; attachments: number; ignored: number };
  notes: {
    total: number;
    new: number;
    updated: number;
    unchanged: number;
    keptLocalEdits: number;
    skippedDeleted: number;
    trashed: number;
    archived: number;
    pinned: number;
    lists: number;
  };
  labels: { total: number; new: string[] };
  attachments: { total: number; new: number; missing: string[]; bytes: number };
  errors: { file: string; error: string }[];
  sample: { title: string; labels: string[]; color: string; kind: string }[];
}

// ---------------------------------------------------------------- parsing

const str = (v: unknown): string => (typeof v === "string" ? v : "");
const num = (v: unknown, d = 0): number => (typeof v === "number" && Number.isFinite(v) ? v : d);
const strs = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : []);
const isObj = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);

function parseItem(v: unknown): ChecklistItem | null {
  if (!isObj(v)) return null;
  return { id: str(v.id) || newItemId(), text: str(v.text), checked: v.checked === true, indent: v.indent === 1 ? 1 : 0 };
}
function parseAttachment(v: unknown): Attachment | null {
  if (!isObj(v) || !str(v.file)) return null;
  const a: Attachment = { id: str(v.id) || str(v.file), file: str(v.file), mimetype: str(v.mimetype), size: num(v.size), createdAt: str(v.createdAt) };
  if (str(v.name)) a.name = str(v.name);
  return a;
}
function parseAnnotation(v: unknown): Annotation | null {
  if (!isObj(v) || !str(v.url)) return null;
  const a: Annotation = { url: str(v.url) };
  if (str(v.title)) a.title = str(v.title);
  if (str(v.description)) a.description = str(v.description);
  if (str(v.source)) a.source = str(v.source);
  return a;
}

export function parseKeepNote(v: unknown): KeepNote {
  const r = isObj(v) ? v : {};
  const color = (KEEP_COLORS as readonly string[]).includes(str(r.color)) ? (str(r.color) as KeepColor) : "default";
  const createdAt = str(r.createdAt);
  const n: KeepNote = {
    id: str(r.id),
    slug: str(r.slug),
    title: str(r.title),
    content: str(r.content),
    tags: strs(r.tags),
    color,
    pinned: r.pinned === true,
    archived: r.archived === true,
    attachments: (Array.isArray(r.attachments) ? r.attachments : []).map(parseAttachment).filter((x): x is Attachment => x !== null),
    annotations: (Array.isArray(r.annotations) ? r.annotations : []).map(parseAnnotation).filter((x): x is Annotation => x !== null),
    collaborators: strs(r.collaborators),
    position: num(r.position, Date.parse(createdAt) || 0),
    rev: num(r.rev),
    source: str(r.source) || "owner",
    createdAt,
    updatedAt: str(r.updatedAt),
  };
  if (Array.isArray(r.items)) n.items = r.items.map(parseItem).filter((x): x is ChecklistItem => x !== null);
  if (isObj(r.reminder) && str(r.reminder.at)) {
    const rec = str(r.reminder.recurrence);
    n.reminder = { at: str(r.reminder.at) };
    if ((RECURRENCES as readonly string[]).includes(rec)) n.reminder.recurrence = rec as Recurrence;
    if (str(r.reminder.lucinaId)) n.reminder.lucinaId = str(r.reminder.lucinaId);
  }
  if (str(r.deletedAt)) n.deletedAt = str(r.deletedAt);
  return n;
}

export function parseChanges(v: unknown): Changes {
  const r = isObj(v) ? v : {};
  return {
    notes: (Array.isArray(r.notes) ? r.notes : []).map(parseKeepNote).filter((n) => n.id),
    removed: strs(r.removed),
    cursor: typeof r.cursor === "number" ? String(r.cursor) : str(r.cursor) || "0",
    hasMore: r.hasMore === true,
  };
}

function parseLabels(v: unknown): LabelInfo[] {
  const arr = isObj(v) && Array.isArray(v.labels) ? v.labels : [];
  const out: LabelInfo[] = [];
  for (const l of arr) if (isObj(l) && str(l.tag)) out.push({ tag: str(l.tag), name: str(l.name) || str(l.tag), count: num(l.count) });
  return out;
}

export function newItemId(): string {
  const bytes = new Uint8Array(6);
  try {
    crypto.getRandomValues(bytes);
  } catch {
    for (let i = 0; i < bytes.length; i++) bytes[i] = Math.floor(Math.random() * 256);
  }
  return `i_${[...bytes].map((b) => b.toString(16).padStart(2, "0")).join("")}`;
}

/** The server's label normalisation: trim, collapse whitespace, drop a leading #, lower-case. */
export function normLabel(name: string): string {
  return name.trim().replace(/^#/, "").replace(/\s+/g, " ").trim().toLowerCase();
}

/** The label name as typed: trimmed, inner whitespace collapsed, a leading # dropped. */
export function cleanLabelName(name: string): string {
  return name.trim().replace(/^#/, "").replace(/\s+/g, " ").trim();
}

// ---------------------------------------------------------------- requests

const enc = (s: string) => encodeURIComponent(s);

export interface KeepApi {
  changes(since: string, limit?: number): Promise<Changes>;
  labels(): Promise<LabelInfo[]>;
  create(input: NoteInput): Promise<KeepNote>;
  patch(id: string, input: NoteInput): Promise<KeepNote>;
  trash(id: string): Promise<void>;
  restore(id: string): Promise<KeepNote>;
  deleteForever(id: string): Promise<void>;
  emptyTrash(): Promise<void>;
  batch(input: BatchInput): Promise<KeepNote[]>;
  createLabel(name: string): Promise<LabelInfo>;
  renameLabel(tag: string, name: string): Promise<LabelInfo>;
  deleteLabel(tag: string): Promise<void>;
  uploadAttachment(id: string, file: Blob, name?: string): Promise<KeepNote>;
  deleteAttachment(id: string, attId: string): Promise<KeepNote>;
}

export const keepApi: KeepApi = {
  changes: async (since, limit = 200) =>
    parseChanges(await request<unknown>("GET", `/v1/notes/changes?since=${enc(since || "0")}&limit=${limit}`)),
  labels: async () => parseLabels(await request<unknown>("GET", "/v1/notes/labels")),
  create: async (input) => parseKeepNote(await request<unknown>("POST", "/v1/notes", { body: input, headers: CLIENT })),
  patch: async (id, input) => parseKeepNote(await request<unknown>("PATCH", `/v1/notes/${enc(id)}`, { body: input, headers: CLIENT })),
  trash: async (id) => {
    await request<unknown>("DELETE", `/v1/notes/${enc(id)}`, { headers: CLIENT });
  },
  restore: async (id) => parseKeepNote(await request<unknown>("POST", `/v1/notes/trash/${enc(id)}/restore`, { headers: CLIENT })),
  deleteForever: async (id) => {
    await request<unknown>("DELETE", `/v1/notes/trash/${enc(id)}`, { headers: CLIENT });
  },
  emptyTrash: async () => {
    await request<unknown>("DELETE", "/v1/notes/trash", { headers: CLIENT });
  },
  batch: async (input) => {
    const r = await request<unknown>("POST", "/v1/notes/batch", { body: input, headers: CLIENT });
    return (isObj(r) && Array.isArray(r.notes) ? r.notes : []).map(parseKeepNote);
  },
  createLabel: async (name) => {
    const l = parseLabels({ labels: [await request<unknown>("POST", "/v1/notes/labels", { body: { name }, headers: CLIENT })] });
    return l[0] ?? { tag: normLabel(name), name, count: 0 };
  },
  renameLabel: async (tag, name) => {
    const l = parseLabels({ labels: [await request<unknown>("PATCH", `/v1/notes/labels/${enc(tag)}`, { body: { name }, headers: CLIENT })] });
    return l[0] ?? { tag: normLabel(name), name, count: 0 };
  },
  deleteLabel: async (tag) => {
    await request<unknown>("DELETE", `/v1/notes/labels/${enc(tag)}`, { headers: CLIENT });
  },
  uploadAttachment: async (id, file, name) =>
    parseKeepNote(
      await request<unknown>("POST", `/v1/notes/${enc(id)}/attachments${name ? `?name=${enc(name)}` : ""}`, {
        raw: { body: file, contentType: file.type || "application/octet-stream" },
        headers: CLIENT,
        timeoutMs: 120_000,
      }),
    ),
  deleteAttachment: async (id, attId) =>
    parseKeepNote(await request<unknown>("DELETE", `/v1/notes/${enc(id)}/attachments/${enc(attId)}`, { headers: CLIENT })),
};

/** The authenticated image URL (the session cookie authorises <img>). */
export function attachmentUrl(a: Pick<Attachment, "file">): string {
  return `/v1/notes/attachments/${a.file}`;
}

export const isImage = (a: Pick<Attachment, "mimetype">): boolean => a.mimetype.startsWith("image/");
export const isAudio = (a: Pick<Attachment, "mimetype">): boolean => a.mimetype.startsWith("audio/");

export const ALLOWED_IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp", "image/heic", "image/heif"];

// ---------------------------------------------------------------- import

export interface ImportOptions {
  dryRun: boolean;
  /** 0..1 of the upload sent. */
  onProgress?: (fraction: number) => void;
  signal?: AbortSignal;
}

/** Upload the Takeout zip with XHR (fetch has no upload progress). */
export function importGoogleKeep(file: Blob, opts: ImportOptions): Promise<ImportReport> {
  return new Promise<ImportReport>((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", `/v1/notes/import/google-keep${opts.dryRun ? "?dryRun=1" : ""}`);
    xhr.withCredentials = true;
    xhr.responseType = "text";
    xhr.timeout = 0;
    xhr.setRequestHeader("X-Secretary-CSRF", "1");
    xhr.setRequestHeader("X-Secretary-Client", "web");
    xhr.setRequestHeader("Accept", "application/json");
    xhr.setRequestHeader("Content-Type", "application/zip");
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable && e.total > 0) opts.onProgress?.(Math.min(1, e.loaded / e.total));
    };
    xhr.onerror = () => reject(new ApiError(0, "network", "Can't reach the server. Check your connection."));
    xhr.ontimeout = () => reject(new ApiError(0, "timeout", "The request took too long."));
    xhr.onabort = () => reject(new ApiError(0, "aborted", "Cancelled."));
    xhr.onload = () => {
      let data: unknown;
      try {
        data = JSON.parse(xhr.responseText);
      } catch {
        data = undefined;
      }
      if (xhr.status >= 200 && xhr.status < 300 && isObj(data)) {
        opts.onProgress?.(1);
        resolve(parseImportReport(data, opts.dryRun));
        return;
      }
      let code = `http_${xhr.status}`;
      let message = `Request failed (${xhr.status}).`;
      if (isObj(data) && isObj(data.error)) {
        if (str(data.error.code)) code = str(data.error.code);
        if (str(data.error.message)) message = str(data.error.message);
      }
      reject(new ApiError(xhr.status, code, message));
    };
    opts.signal?.addEventListener("abort", () => xhr.abort());
    xhr.send(file);
  });
}

export function parseImportReport(v: unknown, dryRun = false): ImportReport {
  const r = isObj(v) ? v : {};
  const files = isObj(r.files) ? r.files : {};
  const notes = isObj(r.notes) ? r.notes : {};
  const labels = isObj(r.labels) ? r.labels : {};
  const atts = isObj(r.attachments) ? r.attachments : {};
  return {
    dryRun: typeof r.dryRun === "boolean" ? r.dryRun : dryRun,
    files: { json: num(files.json), html: num(files.html), attachments: num(files.attachments), ignored: num(files.ignored) },
    notes: {
      total: num(notes.total),
      new: num(notes.new),
      updated: num(notes.updated),
      unchanged: num(notes.unchanged),
      keptLocalEdits: num(notes.keptLocalEdits),
      skippedDeleted: num(notes.skippedDeleted),
      trashed: num(notes.trashed),
      archived: num(notes.archived),
      pinned: num(notes.pinned),
      lists: num(notes.lists),
    },
    labels: { total: num(labels.total), new: strs(labels.new) },
    attachments: { total: num(atts.total), new: num(atts.new), missing: strs(atts.missing), bytes: num(atts.bytes) },
    errors: (Array.isArray(r.errors) ? r.errors : []).filter(isObj).map((e) => ({ file: str(e.file), error: str(e.error) })),
    sample: (Array.isArray(r.sample) ? r.sample : [])
      .filter(isObj)
      .map((s) => ({ title: str(s.title), labels: strs(s.labels), color: str(s.color) || "default", kind: str(s.kind) })),
  };
}
