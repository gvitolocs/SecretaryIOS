import { createEffect, createMemo, createResource, createSignal, Match, on, Show, Switch } from "solid-js";
import { Portal } from "solid-js/web";
import { batch, patchItem, previewUrl, type GalleryItem, type ItemEdit } from "../../api/gallery";
import { t } from "../../i18n";
import { nextRotation } from "../../lib/galleryInfo";
import { supportsJxl } from "../../lib/jxl";
import { PhotoViewer, type ViewerItem } from "../PhotoViewer";
import "../../styles/gallery-viewer.css";
import { AlbumPicker } from "./AlbumPicker";
import { DeleteDialog, type DeleteResult } from "./DeleteDialog";
import { PhotoInfo } from "./PhotoInfo";
import { EditIcon } from "./editor/EditorIcons";
import { PhotoEditor } from "./editor/PhotoEditor";
import { RenameDialog } from "./RenameDialog";
import { AlbumIcon, DownloadIcon, HeartIcon, InfoIcon, RenameIcon, RestoreIcon, RotateIcon, TrashIcon } from "./ViewerIcons";

export interface GalleryViewerProps {
  items: GalleryItem[];
  index: number;
  onIndex: (index: number) => void;
  onClose: () => void;
  /** An edit was saved (favorite, rotation, caption, title, albums): the item as the server has it now. */
  onChanged: (item: GalleryItem) => void;
  /** The item left this list (moved to the Trash, deleted for good, or restored out of the Trash). */
  onRemoved: (id: string) => void;
  /** The list is the Trash: Restore and Delete forever instead of the edit actions. */
  trash?: boolean;
}

type Dialog = "delete" | "album" | "rename" | null;

const INFO_KEY = "gallery.infoOpen";

function storedInfoOpen(): boolean {
  try {
    return localStorage.getItem(INFO_KEY) === "1" && (typeof window.matchMedia !== "function" || window.matchMedia("(min-width: 701px)").matches);
  } catch {
    return false;
  }
}

function rememberInfoOpen(open: boolean): void {
  try {
    localStorage.setItem(INFO_KEY, open ? "1" : "0");
  } catch {
    // Private window or blocked storage: the panel just starts closed next time.
  }
}

/** True for keys typed into a field: the viewer's shortcuts must not take them. */
function typing(target: EventTarget | null): boolean {
  return target instanceof Element && !!target.closest("input, textarea, select, [contenteditable]:not([contenteditable='false'])");
}

/**
 * The Gallery's photos in the shared PhotoViewer, with the info panel (I) and the actions: favorite (F), rotate (R),
 * rename, add to album, download, share and delete (Delete / Backspace). Edits show at once and roll back with an
 * error when the server refuses; every saved change goes out through `onChanged` / `onRemoved`.
 */
export function GalleryViewer(props: GalleryViewerProps) {
  const [jxl] = createResource(supportsJxl);
  const [infoOpen, setInfoOpen] = createSignal(storedInfoOpen());
  const [dialog, setDialog] = createSignal<Dialog>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [editing, setEditing] = createSignal(false);

  // ---- optimistic edits ------------------------------------------------------------------------------------------
  // `base` is the list's item the edit started from: once the list holds another object for that id (the parent applied
  // `onChanged`), the override steps aside. `base: null` is an edit still on its way: always shown.
  interface Override {
    item: GalleryItem;
    base: GalleryItem | null;
  }
  const [overrides, setOverrides] = createSignal<Record<string, Override>>({});
  const pending = new Map<string, number>();
  const queues = new Map<string, Promise<void>>();

  const listed = (id: string) => props.items.find((i) => i.id === id);
  const view = (item: GalleryItem): GalleryItem => {
    const o = overrides()[item.id];
    return o && (o.base === null || o.base === item) ? o.item : item;
  };
  const put = (item: GalleryItem, pinned: boolean) =>
    setOverrides((all) => ({ ...all, [item.id]: { item, base: pinned ? null : (listed(item.id) ?? null) } }));

  const current = createMemo(() => {
    const item = props.items[props.index];
    return item ? view(item) : undefined;
  });

  /** Apply `patch` now, save `request`, and undo just those fields if the server says no. */
  function edit(id: string, patch: Partial<GalleryItem>, request: ItemEdit): Promise<void> {
    const listedItem = listed(id);
    if (!listedItem) return Promise.resolve();
    const before = view(listedItem);
    const keys = Object.keys(patch) as (keyof GalleryItem)[];
    put({ ...before, ...patch }, true);
    pending.set(id, (pending.get(id) ?? 0) + 1);
    setError(null);

    const settle = (): number => {
      const left = (pending.get(id) ?? 1) - 1;
      if (left > 0) pending.set(id, left);
      else pending.delete(id);
      return left;
    };

    const run = async () => {
      try {
        const saved = (await patchItem(id, request)) ?? { ...before, ...patch };
        // While more edits of this item are on their way the optimistic state stays; the last answer settles it.
        if (settle() === 0) put(saved, false);
        props.onChanged(saved);
      } catch (err) {
        const left = settle();
        const now = overrides()[id]?.item ?? before;
        const undone = { ...now } as Record<string, unknown>;
        // Only the fields this edit set, and only while they still hold its value (a later edit may own them now).
        for (const key of keys) if (now[key] === patch[key]) undone[key] = before[key];
        put(undone as unknown as GalleryItem, left > 0);
        setError(t("galleryInfo.actionFailed"));
        throw err;
      }
    };
    // Requests for one item go one at a time, in the order they were made; the first starts at once.
    const prior = queues.get(id);
    const task = prior ? prior.then(run) : run();
    queues.set(id, task.catch(() => undefined));
    return task;
  }

  const favorite = () => {
    const item = current();
    if (!item || props.trash) return;
    void edit(item.id, { favorite: !item.favorite }, { favorite: !item.favorite }).catch(() => undefined);
  };

  const rotate = () => {
    const item = current();
    if (!item || props.trash || item.kind === "video") return;
    const rotation = nextRotation(item.rotation);
    void edit(item.id, { rotation }, { rotation }).catch(() => undefined);
  };

  const saveCaption = (caption: string) => {
    const item = current();
    if (!item) return Promise.resolve();
    return edit(item.id, { caption: caption || null }, { caption });
  };

  const saveRename = async (next: { title: string; caption: string }) => {
    const item = current();
    if (!item) return;
    await edit(item.id, { title: next.title, caption: next.caption || null }, { title: next.title, caption: next.caption });
    setDialog(null);
  };

  const restore = async () => {
    const item = current();
    if (!item) return;
    setError(null);
    try {
      const result = await batch("restore", [item.id]);
      if (!result.done.includes(item.id)) throw new Error(result.failed[0]?.error ?? "failed");
      props.onRemoved(item.id);
    } catch {
      setError(t("galleryInfo.restoreFailed"));
    }
  };

  const deleted = (result: DeleteResult) => {
    setDialog(null);
    for (const id of [...result.trashed, ...result.purged]) props.onRemoved(id);
    if (result.failed.length) setError(t("galleryInfo.deletePartial", { count: result.failed.length }));
  };

  const addedToAlbum = (albumId: string) => {
    setDialog(null);
    const item = current();
    if (!item) return;
    const updated = { ...item, albums: item.albums.includes(albumId) ? item.albums : [...item.albums, albumId] };
    put(updated, false);
    props.onChanged(updated);
  };

  /** The editor saved or reverted: the item as the server has it now (new rendition URLs, `edited`, its size). */
  const edited = (updated: GalleryItem) => {
    setEditing(false);
    put(updated, false);
    props.onChanged(updated);
  };

  const toggleInfo = () => {
    const open = !infoOpen();
    setInfoOpen(open);
    rememberInfoOpen(open);
  };

  // A list that lost its current item (deleted, restored): stay on the one that moved up, close when none is left.
  createEffect(
    on(
      () => props.items.length,
      (length) => {
        if (length === 0) props.onClose();
        else if (props.index >= length) props.onIndex(length - 1);
      },
      { defer: true },
    ),
  );

  // ---- keys ------------------------------------------------------------------------------------------------------

  const onKeyDown = (e: KeyboardEvent): boolean => {
    // A dialog owns the keyboard; so does a text field, and the editor over the viewer.
    if (editing() || dialog() || typing(e.target)) return true;
    if (e.metaKey || e.ctrlKey || e.altKey) return false;
    switch (e.key) {
      case "i":
      case "I":
        e.preventDefault();
        toggleInfo();
        return true;
      case "f":
      case "F":
        e.preventDefault();
        favorite();
        return true;
      case "r":
      case "R":
        e.preventDefault();
        rotate();
        return true;
      case "e":
      case "E":
        e.preventDefault();
        if (current()?.kind === "photo" && !props.trash) setEditing(true);
        return true;
      case "Delete":
      case "Backspace":
        e.preventDefault();
        if (current()) setDialog("delete");
        return true;
      default:
        return false;
    }
  };

  // ---- what the shared viewer shows ------------------------------------------------------------------------------

  const items = createMemo<ViewerItem[]>(() =>
    props.items.map((raw) => {
      const item = view(raw);
      return {
        src: previewUrl(item, jxl() ?? false),
        // Zooming past the preview loads the full-size picture: the edit's derivative when there is one.
        original: item.urls.edited ?? item.urls.original,
        name: item.title || item.origin.filename || "",
        mime: item.mime,
        kind: item.kind,
        rotation: item.rotation,
      };
    }),
  );

  // Built once: the buttons read the current item as it changes, so a click never replaces the button that has focus.
  const Toolbar = () => (
    <Show when={current()}>
      {(item) => (
        <>
          <button
            type="button"
            class="pv-btn"
            classList={{ "pv-btn--active": infoOpen() }}
            aria-label={t("galleryInfo.info")}
            aria-pressed={infoOpen()}
            title={`${t("galleryInfo.info")} (I)`}
            onClick={toggleInfo}
          >
            <InfoIcon />
          </button>
          <Show when={!props.trash}>
            <button
              type="button"
              class="pv-btn"
              classList={{ "pv-btn--on": item().favorite }}
              aria-label={item().favorite ? t("galleryInfo.unfavorite") : t("galleryInfo.favorite")}
              aria-pressed={item().favorite}
              title={`${item().favorite ? t("galleryInfo.unfavorite") : t("galleryInfo.favorite")} (F)`}
              onClick={favorite}
            >
              <HeartIcon filled={item().favorite} />
            </button>
            <Show when={item().kind === "photo"}>
              <button type="button" class="pv-btn" aria-label={t("galleryInfo.rotate")} title={`${t("galleryInfo.rotate")} (R)`} onClick={rotate}>
                <RotateIcon />
              </button>
            </Show>
            <Show when={item().kind === "photo"}>
              <button type="button" class="pv-btn" aria-label={item().edited ? t("editor.editEdited") : t("editor.edit")} title={`${item().edited ? t("editor.editEdited") : t("editor.edit")} (E)`} onClick={() => setEditing(true)}>
                <EditIcon />
              </button>
            </Show>
            <button type="button" class="pv-btn" aria-label={t("galleryInfo.rename")} title={t("galleryInfo.rename")} onClick={() => setDialog("rename")}>
              <RenameIcon />
            </button>
            <button type="button" class="pv-btn" aria-label={t("galleryInfo.addToAlbum")} title={t("galleryInfo.addToAlbum")} onClick={() => setDialog("album")}>
              <AlbumIcon />
            </button>
          </Show>
          <Show when={item().urls.download ?? item().urls.original}>
            {(href) => (
              <a class="pv-btn" href={href()} download={item().origin.filename ?? item().title ?? ""} aria-label={t("galleryInfo.download")} title={t("galleryInfo.download")}>
                <DownloadIcon />
              </a>
            )}
          </Show>
          <Show when={item().urls.downloadEdited}>
            {(href) => (
              <a class="pv-btn" href={href()} download="" aria-label={t("editor.downloadEdited")} title={t("editor.downloadEdited")}>
                <DownloadIcon />
              </a>
            )}
          </Show>
          <Show when={props.trash}>
            <button type="button" class="pv-btn" aria-label={t("galleryInfo.restore")} title={t("galleryInfo.restore")} onClick={() => void restore()}>
              <RestoreIcon />
            </button>
          </Show>
          <button
            type="button"
            class="pv-btn pv-btn--danger"
            aria-label={props.trash ? t("galleryInfo.deleteForever") : t("galleryInfo.delete")}
            title={`${props.trash ? t("galleryInfo.deleteForever") : t("galleryInfo.delete")} (Delete)`}
            onClick={() => setDialog("delete")}
          >
            <TrashIcon />
          </button>
        </>
      )}
    </Show>
  );

  const Panel = () => (
    <Show when={current()}>
      {(item) => <PhotoInfo item={item()} trash={props.trash} onClose={toggleInfo} onCaption={saveCaption} />}
    </Show>
  );

  return (
    <>
      <PhotoViewer
        items={items()}
        index={props.index}
        onIndex={props.onIndex}
        onClose={props.onClose}
        actions={{ share: true }}
        toolbar={<Toolbar />}
        panel={infoOpen() ? <Panel /> : undefined}
        onKeyDown={onKeyDown}
      />
      <Show when={error()}>
        {(message) => (
          <Portal>
            <div class="gv-toast" classList={{ "gv-toast--panel": infoOpen() }} role="alert">
              <span>{message()}</span>
              <button type="button" onClick={() => setError(null)}>
                {t("galleryInfo.dismissError")}
              </button>
            </div>
          </Portal>
        )}
      </Show>
      <Show when={editing() && current()}>{(item) => <PhotoEditor item={item()} onClose={() => setEditing(false)} onSaved={edited} />}</Show>
      <Show when={current()}>
        {(item) => (
          <Switch>
            <Match when={dialog() === "delete"}>
              <DeleteDialog items={[item()]} purge={props.trash} onDone={deleted} onCancel={() => setDialog(null)} />
            </Match>
            <Match when={dialog() === "album"}>
              <AlbumPicker itemIds={[item().id]} onDone={addedToAlbum} onCancel={() => setDialog(null)} />
            </Match>
            <Match when={dialog() === "rename"}>
              <RenameDialog item={item()} onSave={saveRename} onCancel={() => setDialog(null)} />
            </Match>
          </Switch>
        )}
      </Show>
    </>
  );
}
