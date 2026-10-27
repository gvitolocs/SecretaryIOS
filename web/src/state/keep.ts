// The Keep store: the note cache (memory + IndexedDB), the sync cursor and optimistic writes.
//
// Sync (keep-contract.md, "Sync (clients)"): render the cache, then GET /v1/notes/changes?since=cursor
// until hasMore is false, again on focus / visibility and every 60 s while visible; 410 cursor_expired
// drops the cache and loads from 0. Writes apply locally first, go to the server in order per note,
// and the server's answer replaces the local copy. A note with a write in flight ignores incoming
// sync data until its answer arrives (the answer is newer than what the sync saw).

import { batch, createMemo, createRoot, createSignal, type Accessor } from "solid-js";
import { ApiError, toApiError } from "../api/client";
import {
  cleanLabelName,
  keepApi,
  newItemId,
  normLabel,
  type BatchInput,
  type ChecklistItem,
  type KeepApi,
  type KeepNote,
  type LabelInfo,
  type NoteInput,
} from "../api/keep";
import { keepDb, type KeepDb } from "../lib/keepDb";
import { sortLabels } from "../lib/keepFilter";

export type SyncStatus = "idle" | "syncing" | "error";

export interface KeepStoreDeps {
  api: KeepApi;
  db: KeepDb;
  now?: () => number;
  /** A message for the snackbar (errors the owner should see). */
  notify?: (message: string) => void;
  /** Where attachment bytes come from when a note is copied. */
  fetchBlob?: (url: string) => Promise<Blob>;
}

/** The checklist as markdown lines, the way the server derives `content` for a list note. */
export function itemsToContent(items: ChecklistItem[]): string {
  return items.map((i) => `${i.indent === 1 ? "  " : ""}- [${i.checked ? "x" : " "}] ${i.text}`).join("\n");
}

/** The lines of a text as new unchecked checklist items (checkbox syntax keeps its state). */
export function contentToItems(content: string): ChecklistItem[] {
  const items: ChecklistItem[] = [];
  for (const raw of content.split(/\r?\n/)) {
    if (!raw.trim()) continue;
    const m = /^(\s*)[-*]\s+\[([ xX])\]\s?(.*)$/.exec(raw);
    if (m) items.push({ id: newItemId(), text: (m[3] ?? "").trim(), checked: m[2] !== " ", indent: (m[1]?.length ?? 0) >= 2 ? 1 : 0 });
    else items.push({ id: newItemId(), text: raw.trim(), checked: false, indent: 0 });
  }
  return items;
}

/** A note with a patch applied locally, the way the server will apply it (optimistic writes). */
export function applyInput(note: KeepNote, input: NoteInput, nowIso: string): KeepNote {
  const n: KeepNote = { ...note, updatedAt: nowIso };
  if (input.title !== undefined) n.title = input.title;
  if (input.content !== undefined) n.content = input.content;
  if (input.color !== undefined) n.color = input.color;
  if (input.pinned !== undefined) n.pinned = input.pinned;
  if (input.archived !== undefined) n.archived = input.archived;
  if (input.position !== undefined) n.position = input.position;
  if (input.collaborators !== undefined) n.collaborators = [...input.collaborators];
  if (input.labels !== undefined) n.tags = uniqueTags(input.labels);
  if (input.reminder === null) delete n.reminder;
  else if (input.reminder !== undefined) n.reminder = { ...input.reminder };
  if (input.items !== undefined) {
    n.items = input.items.map((i) => ({ ...i }));
    n.content = itemsToContent(n.items);
  }
  if (input.convert === "text" && n.items) {
    n.content = n.items.map((i) => i.text).join("\n");
    delete n.items;
  } else if (input.convert === "list" && !n.items) {
    n.items = contentToItems(n.content);
    n.content = itemsToContent(n.items);
  }
  return n;
}

function uniqueTags(names: string[]): string[] {
  const out: string[] = [];
  for (const name of names) {
    const t = normLabel(name);
    if (t && !out.includes(t)) out.push(t);
  }
  return out;
}

const iso = (ms: number) => new Date(ms).toISOString();

export function createKeepStore(deps: KeepStoreDeps) {
  const { api, db } = deps;
  const now = deps.now ?? (() => Date.now());
  const notify = deps.notify ?? (() => undefined);

  const notes = new Map<string, KeepNote>();
  const alias = new Map<string, string>();
  const pending = new Map<string, number>();
  const snapshots = new Map<string, KeepNote | null>();
  const chains = new Map<string, Promise<unknown>>();
  let cursor = "0";
  let tmpSeq = 0;

  const [version, setVersion] = createSignal(0);
  const [registry, setRegistry] = createSignal<LabelInfo[]>([]);
  const [status, setStatus] = createSignal<SyncStatus>("idle");
  const [loaded, setLoaded] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [uploading, setUploading] = createSignal<Record<string, number>>({});
  const bump = () => setVersion((v) => v + 1);

  const resolveId = (id: string): string => alias.get(id) ?? id;

  const all: Accessor<KeepNote[]> = createRoot(() =>
    createMemo(() => {
      version();
      return [...notes.values()];
    }),
  );

  /** Registry names plus labels in use, with live counts, alphabetical like Keep. */
  const labels: Accessor<LabelInfo[]> = createRoot(() =>
    createMemo(() => {
      const counts = new Map<string, number>();
      for (const n of all()) if (!n.deletedAt) for (const t of n.tags) counts.set(t, (counts.get(t) ?? 0) + 1);
      const out = new Map<string, LabelInfo>();
      for (const l of registry()) out.set(l.tag, { tag: l.tag, name: l.name, count: counts.get(l.tag) ?? 0 });
      for (const [tag, count] of counts) if (!out.has(tag)) out.set(tag, { tag, name: tag, count });
      return sortLabels([...out.values()]);
    }),
  );

  function labelName(tag: string): string {
    return registry().find((l) => l.tag === tag)?.name ?? tag;
  }

  function note(id: string): KeepNote | undefined {
    version();
    return notes.get(resolveId(id));
  }

  // ------------------------------------------------------------ sync

  function merge(notesIn: KeepNote[], removed: string[]): void {
    const put: KeepNote[] = [];
    const gone: string[] = [];
    for (const n of notesIn) {
      if (pending.get(n.id)) continue;
      const local = notes.get(n.id);
      if (local && local.rev > n.rev) continue;
      notes.set(n.id, n);
      put.push(n);
    }
    for (const id of removed) {
      if (pending.get(id)) continue;
      notes.delete(id);
      gone.push(id);
    }
    if (put.length || gone.length) {
      bump();
      void db.putNotes(put);
      void db.removeNotes(gone);
    }
  }

  async function dropCache(): Promise<void> {
    for (const id of [...notes.keys()]) if (!id.startsWith("tmp_") && !pending.get(id)) notes.delete(id);
    cursor = "0";
    bump();
    await db.clear();
  }

  async function syncOnce(): Promise<boolean> {
    let since = cursor;
    let changed = false;
    let reloaded = false;
    for (;;) {
      let ch;
      try {
        ch = await api.changes(since);
      } catch (e) {
        if (e instanceof ApiError && e.status === 410 && !reloaded) {
          reloaded = true;
          await dropCache();
          since = "0";
          changed = true;
          continue;
        }
        throw e;
      }
      batch(() => merge(ch.notes, ch.removed));
      if (ch.notes.length || ch.removed.length) changed = true;
      since = ch.cursor;
      cursor = since;
      await db.setCursor(since);
      if (!ch.hasMore) break;
    }
    return changed;
  }

  let running: Promise<void> | undefined;
  let again = false;
  let firstSync = true;

  async function refreshLabels(): Promise<void> {
    try {
      const l = await api.labels();
      setRegistry(l);
      await db.setLabels(l);
    } catch {
      // The registry only adds display case; the tags in use still show.
    }
  }

  /** Pull what changed since the cursor. Overlapping calls share one run (plus one more if asked meanwhile). */
  function sync(): Promise<void> {
    if (running) {
      again = true;
      return running;
    }
    running = (async () => {
      setStatus("syncing");
      try {
        do {
          again = false;
          // The first run asks for the label names at the same time, so the nav shows them with the notes.
          const labelsNow = firstSync ? refreshLabels() : undefined;
          firstSync = false;
          const changed = await syncOnce();
          await labelsNow;
          if (changed) await refreshLabels();
        } while (again);
        setStatus("idle");
        setError(null);
      } catch (e) {
        setStatus("error");
        setError(toApiError(e).message);
      } finally {
        running = undefined;
      }
    })();
    return running;
  }

  /** Render the cache at once, then sync. */
  async function load(): Promise<void> {
    if (!loaded()) {
      const c = await db.load();
      batch(() => {
        for (const n of c.notes) if (!notes.has(n.id)) notes.set(n.id, n);
        cursor = c.cursor;
        setRegistry(c.labels);
        bump();
        setLoaded(true);
      });
    }
    await sync();
  }

  // ------------------------------------------------------------ writes

  function enqueue<T>(ids: string[], fn: () => Promise<T>): Promise<T> {
    const prior = Promise.all(ids.map((id) => chains.get(id) ?? Promise.resolve()));
    const p = prior.then(fn, fn);
    const tail = p.then(
      () => undefined,
      () => undefined,
    );
    for (const id of ids) chains.set(id, tail);
    return p;
  }

  function begin(id: string): void {
    const n = pending.get(id) ?? 0;
    if (n === 0) snapshots.set(id, notes.get(id) ?? null);
    pending.set(id, n + 1);
  }

  /** Mark one write finished; true when it was the last one in flight for that note. */
  function finish(id: string): boolean {
    const n = (pending.get(id) ?? 1) - 1;
    if (n <= 0) {
      pending.delete(id);
      return true;
    }
    pending.set(id, n);
    return false;
  }

  function accept(saved: KeepNote): void {
    notes.set(saved.id, saved);
    void db.putNotes([saved]);
  }

  /** Apply `local` to the notes now, then run `remote` in order; on failure restore the earlier copy. */
  function mutate(ids: string[], local: (n: KeepNote) => KeepNote | null, remote: (realIds: string[]) => Promise<KeepNote[] | void>): Promise<void> {
    const keys = ids.map(resolveId).filter((id) => notes.has(id));
    if (!keys.length) return Promise.resolve();
    batch(() => {
      for (const id of keys) {
        begin(id);
        const cur = notes.get(id)!;
        const next = local(cur);
        if (next) notes.set(id, next);
        else notes.delete(id);
      }
      bump();
    });
    return enqueue(ids, async () => {
      const real = keys.map(resolveId);
      const settle = (): { id: string; last: boolean }[] => keys.map((k) => ({ id: resolveId(k), last: finish(resolveId(k)) }));
      // The note's creation failed (its temporary copy is gone): there is nothing to write to.
      if (real.some((id) => id.startsWith("tmp_"))) {
        settle();
        return;
      }
      try {
        const saved = (await remote(real)) ?? [];
        batch(() => {
          for (const { id, last } of settle()) {
            const answer = saved.find((s) => s.id === id);
            if (last && answer) accept(answer);
            if (last) snapshots.delete(id);
          }
          bump();
        });
        if (!saved.length) void sync();
      } catch (e) {
        const err = toApiError(e);
        batch(() => {
          for (const { id, last } of settle()) {
            if (!last) continue;
            const snap = snapshots.get(id);
            snapshots.delete(id);
            if (err.status === 404) {
              notes.delete(id);
              void db.removeNotes([id]);
            } else if (snap) notes.set(id, snap);
            else notes.delete(id);
          }
          bump();
        });
        notify(err.status === 0 ? "Can't reach the server. Your change was not saved." : err.message);
      }
    });
  }

  function blankNote(id: string, input: NoteInput): KeepNote {
    const ms = now();
    const base: KeepNote = {
      id,
      slug: "",
      title: "",
      content: "",
      tags: [],
      color: "default",
      pinned: false,
      archived: false,
      attachments: [],
      annotations: [],
      collaborators: [],
      position: ms,
      rev: 0,
      source: "web",
      createdAt: iso(ms),
      updatedAt: iso(ms),
    };
    return applyInput(base, input, iso(ms));
  }

  /** Create a note: it shows at once (id tmp_…) and is replaced by the server's copy. */
  function create(input: NoteInput): { id: string; saved: Promise<KeepNote | null> } {
    const tmp = `tmp_${now().toString(36)}${(tmpSeq++).toString(36)}`;
    const local = blankNote(tmp, input);
    notes.set(tmp, local);
    begin(tmp);
    bump();
    const saved = enqueue([tmp], async () => {
      try {
        const real = await api.create(input);
        batch(() => {
          const local = notes.get(tmp);
          const queued = (pending.get(tmp) ?? 1) - 1;
          notes.delete(tmp);
          pending.delete(tmp);
          snapshots.delete(tmp);
          alias.set(tmp, real.id);
          if (queued > 0 && local) {
            // Edits made while the create was in flight are queued behind it: keep them on screen.
            notes.set(real.id, { ...local, id: real.id, slug: real.slug, rev: real.rev, createdAt: real.createdAt });
            pending.set(real.id, queued);
            snapshots.set(real.id, real);
            void db.putNotes([real]);
          } else accept(real);
          bump();
        });
        return real;
      } catch (e) {
        batch(() => {
          notes.delete(tmp);
          pending.delete(tmp);
          snapshots.delete(tmp);
          bump();
        });
        notify(toApiError(e).message);
        return null;
      }
    });
    return { id: tmp, saved };
  }

  function update(id: string, input: NoteInput): Promise<void> {
    return updateMany([id], input);
  }

  function updateMany(ids: string[], input: NoteInput): Promise<void> {
    const stamp = iso(now());
    return mutate(
      ids,
      (n) => applyInput(n, input, stamp),
      async (real) => {
        const out: KeepNote[] = [];
        for (const id of real) out.push(await api.patch(id, input));
        return out;
      },
    );
  }

  function setMany(ids: string[], set: NonNullable<BatchInput["set"]>): Promise<void> {
    const stamp = iso(now());
    return mutate(
      ids,
      (n) => applyInput(n, set, stamp),
      (real) => api.batch({ ids: real, set }),
    );
  }

  function addLabel(ids: string[], name: string): Promise<void> {
    const clean = cleanLabelName(name);
    const tag = normLabel(clean);
    if (!tag) return Promise.resolve();
    if (!registry().some((l) => l.tag === tag)) setRegistry([...registry(), { tag, name: clean, count: 0 }]);
    return mutate(
      ids,
      (n) => (n.tags.includes(tag) ? n : { ...n, tags: [...n.tags, tag], updatedAt: iso(now()) }),
      (real) => api.batch({ ids: real, addLabels: [clean] }),
    );
  }

  function removeLabel(ids: string[], tag: string): Promise<void> {
    return mutate(
      ids,
      (n) => ({ ...n, tags: n.tags.filter((t) => t !== tag), updatedAt: iso(now()) }),
      (real) => api.batch({ ids: real, removeLabels: [labelName(tag)] }),
    );
  }

  function trash(ids: string[]): Promise<void> {
    const stamp = iso(now());
    return mutate(
      ids,
      (n) => ({ ...n, deletedAt: stamp }),
      (real) => api.batch({ ids: real, trash: true }),
    );
  }

  function restore(ids: string[]): Promise<void> {
    return mutate(
      ids,
      (n) => {
        const t = { ...n };
        delete t.deletedAt;
        return t;
      },
      (real) => api.batch({ ids: real, restore: true }),
    );
  }

  function deleteForever(ids: string[]): Promise<void> {
    return mutate(
      ids,
      () => null,
      async (real) => {
        await api.batch({ ids: real, deleteForever: true });
        void db.removeNotes(real);
      },
    );
  }

  function emptyTrash(): Promise<void> {
    const ids = [...notes.values()].filter((n) => n.deletedAt).map((n) => n.id);
    if (!ids.length) return Promise.resolve();
    return mutate(
      ids,
      () => null,
      async () => {
        await api.emptyTrash();
        void db.removeNotes(ids);
      },
    );
  }

  // ------------------------------------------------------------ attachments

  function bumpUploading(id: string, by: number): void {
    setUploading((u) => {
      const next = { ...u, [id]: Math.max(0, (u[id] ?? 0) + by) };
      if (!next[id]) delete next[id];
      return next;
    });
  }

  function uploadImages(id: string, files: File[]): Promise<void> {
    if (!files.length) return Promise.resolve();
    bumpUploading(id, files.length);
    return enqueue([id], async () => {
      const real = resolveId(id);
      for (const f of files) {
        try {
          const saved = await api.uploadAttachment(real, f, f.name);
          batch(() => {
            accept(saved);
            bump();
          });
        } catch (e) {
          notify(toApiError(e).message);
        } finally {
          bumpUploading(id, -1);
        }
      }
    });
  }

  function removeAttachment(id: string, attId: string): Promise<void> {
    return mutate(
      [id],
      (n) => ({ ...n, attachments: n.attachments.filter((a) => a.id !== attId), updatedAt: iso(now()) }),
      async (real) => [await api.deleteAttachment(real[0]!, attId)],
    );
  }

  /** "Make a copy": same text, list, colour, labels and images, as a new note on top. */
  async function duplicate(id: string): Promise<string | null> {
    const src = notes.get(resolveId(id));
    if (!src) return null;
    const title = src.title ? `Copy of ${src.title}` : "";
    const input: NoteInput = {
      title,
      content: src.items ? undefined : src.content,
      items: src.items?.map((i) => ({ ...i, id: newItemId() })),
      color: src.color,
      labels: src.tags.map(labelName),
    };
    if (input.content === undefined) delete input.content;
    const made = create(input);
    const real = await made.saved;
    if (real && src.attachments.length) {
      const getBlob = deps.fetchBlob ?? (async (url: string) => (await fetch(url, { credentials: "same-origin" })).blob());
      const files: File[] = [];
      for (const a of src.attachments) {
        try {
          const blob = await getBlob(`/v1/notes/attachments/${a.file}`);
          files.push(new File([blob], a.name ?? a.file, { type: a.mimetype }));
        } catch {
          // The copy keeps the text; an image that cannot be read is skipped.
        }
      }
      await uploadImages(real.id, files);
    }
    return real?.id ?? null;
  }

  // ------------------------------------------------------------ labels

  async function createLabel(name: string): Promise<LabelInfo | null> {
    const clean = cleanLabelName(name);
    if (!normLabel(clean)) return null;
    try {
      const l = await api.createLabel(clean);
      setRegistry([...registry().filter((x) => x.tag !== l.tag), l]);
      void db.setLabels(registry());
      return l;
    } catch (e) {
      const err = toApiError(e);
      if (err.code !== "label_exists") notify(err.message);
      return null;
    }
  }

  async function renameLabel(tag: string, name: string): Promise<void> {
    const clean = cleanLabelName(name);
    if (!normLabel(clean)) return;
    try {
      await api.renameLabel(tag, clean);
      await Promise.all([refreshLabels(), sync()]);
    } catch (e) {
      notify(toApiError(e).message);
    }
  }

  async function deleteLabel(tag: string): Promise<void> {
    try {
      await api.deleteLabel(tag);
      batch(() => {
        for (const [id, n] of notes) if (n.tags.includes(tag)) notes.set(id, { ...n, tags: n.tags.filter((t) => t !== tag) });
        setRegistry(registry().filter((l) => l.tag !== tag));
        bump();
      });
      await Promise.all([refreshLabels(), sync()]);
    } catch (e) {
      notify(toApiError(e).message);
    }
  }

  /** Sign-out: forget everything. */
  async function reset(): Promise<void> {
    notes.clear();
    alias.clear();
    pending.clear();
    snapshots.clear();
    chains.clear();
    cursor = "0";
    firstSync = true;
    batch(() => {
      setRegistry([]);
      setLoaded(false);
      setStatus("idle");
      bump();
    });
    await db.clear();
  }

  return {
    all,
    labels,
    labelName,
    note,
    status,
    error,
    loaded,
    uploading,
    version,
    cursor: () => cursor,
    load,
    sync,
    create,
    update,
    updateMany,
    setMany,
    addLabel,
    removeLabel,
    trash,
    restore,
    deleteForever,
    emptyTrash,
    uploadImages,
    removeAttachment,
    duplicate,
    createLabel,
    renameLabel,
    deleteLabel,
    reset,
    pendingCount: () => pending.size,
  };
}

export type KeepStore = ReturnType<typeof createKeepStore>;

/** Sync on focus / when the tab becomes visible, and every `intervalMs` while visible. Returns the cleanup. */
export function startAutoSync(store: Pick<KeepStore, "sync">, intervalMs = 60_000): () => void {
  const visible = () => typeof document === "undefined" || document.visibilityState !== "hidden";
  const tick = () => {
    if (visible()) void store.sync();
  };
  const timer = setInterval(tick, intervalMs);
  document.addEventListener("visibilitychange", tick);
  window.addEventListener("focus", tick);
  window.addEventListener("online", tick);
  return () => {
    clearInterval(timer);
    document.removeEventListener("visibilitychange", tick);
    window.removeEventListener("focus", tick);
    window.removeEventListener("online", tick);
  };
}

// ---------------------------------------------------------------- snackbar

export interface SnackbarMessage {
  id: number;
  text: string;
  action?: { label: string; run: () => void };
}

const [snackbar, setSnackbar] = createSignal<SnackbarMessage | null>(null);
let snackSeq = 0;
let snackTimer: ReturnType<typeof setTimeout> | undefined;
export { snackbar };

/** One message at a time, like Keep's bottom-left snackbar; an action ("Undo") closes it. */
export function showSnackbar(text: string, action?: SnackbarMessage["action"], ms = 6500): void {
  clearTimeout(snackTimer);
  setSnackbar({ id: ++snackSeq, text, ...(action ? { action } : {}) });
  snackTimer = setTimeout(() => setSnackbar(null), ms);
}

export function dismissSnackbar(): void {
  clearTimeout(snackTimer);
  setSnackbar(null);
}

// ---------------------------------------------------------------- the app's store

export const keep: KeepStore = createRoot(() => createKeepStore({ api: keepApi, db: keepDb, notify: (m) => showSnackbar(m) }));

export function resetKeep(): void {
  dismissSnackbar();
  void keep.reset();
}
