import { createResource, createSignal, For, Show } from "solid-js";
import { batch, createAlbum, listAlbums, type GalleryAlbum } from "../../api/gallery";
import { t } from "../../i18n";
import { Modal } from "./Modal";

export interface AlbumPickerProps {
  /** The items to add (one or many). */
  itemIds: string[];
  /** The items are in this album now. */
  onDone: (albumId: string) => void;
  onCancel: () => void;
}

/** Pick one of the Gallery's albums, or create one, and add the items to it. */
export function AlbumPicker(props: AlbumPickerProps) {
  const [albums, { refetch }] = createResource(listAlbums);
  const [name, setName] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  // The built-in albums (Favorites, Videos…) fill themselves; only the owner's own can be added to.
  const own = () => (albums() ?? []).filter((a) => !a.system);

  const add = async (album: GalleryAlbum) => {
    const result = await batch("album_add", props.itemIds, { album: album.id });
    if (!result.done.length && result.failed.length) throw new Error(result.failed[0]?.error ?? "failed");
    props.onDone(album.id);
  };

  const pick = async (album: GalleryAlbum) => {
    if (busy()) return;
    setBusy(true);
    setError(null);
    try {
      await add(album);
    } catch {
      setError(t("galleryInfo.albumAddFailed"));
      setBusy(false);
    }
  };

  const create = async (e: SubmitEvent) => {
    e.preventDefault();
    const title = name().trim();
    if (!title || busy()) return;
    setBusy(true);
    setError(null);
    let album: GalleryAlbum;
    try {
      album = await createAlbum(title);
    } catch {
      setError(t("galleryInfo.albumCreateFailed"));
      setBusy(false);
      return;
    }
    try {
      await add(album);
    } catch {
      // The album exists now; keep it in the list so a retry picks it instead of making a second one.
      void refetch();
      setName("");
      setError(t("galleryInfo.albumAddFailed"));
      setBusy(false);
    }
  };

  return (
    <Modal labelledBy="gv-album-title" busy={busy()} onClose={props.onCancel}>
      <h2 id="gv-album-title" class="gv-dialog-title">
        {t("galleryInfo.albumTitle")}
      </h2>
      <p class="gv-dialog-sub">{t("galleryInfo.albumFor", { count: props.itemIds.length })}</p>
      <div class="gv-dialog-body">
        <Show when={!albums.loading} fallback={<p class="gv-muted" role="status">{t("galleryInfo.albumsLoading")}</p>}>
          <Show when={!albums.error} fallback={<p class="gv-error" role="alert">{t("galleryInfo.albumsError")}</p>}>
            <Show when={own().length} fallback={<p class="gv-muted">{t("galleryInfo.albumsEmpty")}</p>}>
              <ul class="gv-albums" aria-label={t("galleryInfo.albumTitle")}>
                <For each={own()}>
                  {(album) => (
                    <li>
                      <button type="button" class="gv-album" disabled={busy()} onClick={() => void pick(album)}>
                        <span class="gv-album-title">{album.title}</span>
                        <span class="gv-album-count">{t("galleryInfo.albumCount", { count: album.count })}</span>
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </Show>
        </Show>
        <form class="gv-new-album" onSubmit={(e) => void create(e)}>
          <label class="field-label" for="gv-new-album">
            {t("galleryInfo.newAlbum")}
          </label>
          <div class="gv-new-album-row">
            <input
              id="gv-new-album"
              class="text-input"
              type="text"
              maxLength={120}
              placeholder={t("galleryInfo.newAlbumPlaceholder")}
              value={name()}
              disabled={busy()}
              onInput={(e) => setName(e.currentTarget.value)}
            />
            <button type="submit" class="btn btn--primary" disabled={busy() || !name().trim()}>
              {t("galleryInfo.createAlbum")}
            </button>
          </div>
        </form>
        <Show when={error()}>
          {(message) => (
            <p class="gv-error" role="alert">
              {message()}
            </p>
          )}
        </Show>
      </div>
      <div class="gv-dialog-actions">
        <button type="button" class="btn" disabled={busy()} onClick={() => props.onCancel()}>
          {t("common.cancel")}
        </button>
      </div>
    </Modal>
  );
}
