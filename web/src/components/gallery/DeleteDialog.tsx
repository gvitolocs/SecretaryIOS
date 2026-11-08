import { createMemo, createSignal, Show } from "solid-js";
import { batch, type BatchResult, type GalleryItem } from "../../api/gallery";
import { locale, t } from "../../i18n";
import { deviceNames, joinNames, TRASH_DAYS } from "../../lib/galleryInfo";
import { Modal } from "./Modal";

export interface DeleteResult {
  /** Moved to the Trash. */
  trashed: string[];
  /** Deleted for good (purge mode). */
  purged: string[];
  /** The server refused these (or could not do them). */
  failed: { id: string; error: string }[];
}

export interface DeleteDialogProps {
  items: GalleryItem[];
  /** Called once the server has done it (all or only some: see `failed`). */
  onDone: (result: DeleteResult) => void;
  onCancel: () => void;
  /** The items are in the Trash: delete them forever, after a stronger confirmation. */
  purge?: boolean;
}

/**
 * Delete for one item or many. In Lucina this only moves to the Trash (emptied after 30 days); the originals on the
 * phone or Mac stay, unless "Also delete from <device>" is ticked, which asks the device to remove them (the device
 * asks the owner to confirm). In the Trash, "Delete forever" removes the items from Lucina and needs a tick first.
 */
export function DeleteDialog(props: DeleteDialogProps) {
  const [alsoDevice, setAlsoDevice] = createSignal(false);
  const [understood, setUnderstood] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const count = () => props.items.length;
  const devices = createMemo(() => joinNames(deviceNames(props.items), locale()));
  const hasDevice = () => deviceNames(props.items).length > 0;
  const ids = () => props.items.map((i) => i.id);

  const confirm = async () => {
    if (busy() || !count()) return;
    if (props.purge && !understood()) return;
    setBusy(true);
    setError(null);
    try {
      const result: BatchResult = props.purge
        ? await batch("purge", ids())
        : await batch("trash", ids(), { alsoFromDevice: alsoDevice() && hasDevice() });
      if (!result.done.length && result.failed.length) {
        setError(t("galleryInfo.deletePartial", { count: result.failed.length }));
        setBusy(false);
        return;
      }
      props.onDone({
        trashed: props.purge ? [] : result.done,
        purged: props.purge ? result.done : [],
        failed: result.failed,
      });
    } catch {
      setError(t("galleryInfo.deleteFailed"));
      setBusy(false);
    }
  };

  return (
    <Modal
      labelledBy="gv-delete-title"
      describedBy="gv-delete-body"
      busy={busy()}
      onClose={props.onCancel}
      initialFocus={() => document.getElementById("gv-delete-cancel")}
    >
      <h2 id="gv-delete-title" class="gv-dialog-title">
        {t(props.purge ? "galleryInfo.purgeTitle" : "galleryInfo.trashTitle", { count: count() })}
      </h2>
      <div id="gv-delete-body" class="gv-dialog-body">
        <Show
          when={props.purge}
          fallback={
            <>
              <p>{t("galleryInfo.trashLine", { count: count() })}</p>
              <Show when={hasDevice()}>
                <p>{t("galleryInfo.originalsStay", { devices: devices() })}</p>
              </Show>
              <p>{t("galleryInfo.trashEmptied", { days: TRASH_DAYS })}</p>
              <Show when={hasDevice()}>
                <label class="gv-check">
                  <input type="checkbox" checked={alsoDevice()} disabled={busy()} onChange={(e) => setAlsoDevice(e.currentTarget.checked)} />
                  <span>
                    <span class="gv-check-label">{t("galleryInfo.alsoFromDevice", { devices: devices() })}</span>
                    <span class="gv-check-hint">{t("galleryInfo.alsoFromDeviceHint")}</span>
                  </span>
                </label>
              </Show>
            </>
          }
        >
          <p>{t("galleryInfo.purgeLine", { count: count() })}</p>
          <p>{t("galleryInfo.purgeKeeps")}</p>
          <label class="gv-check">
            <input type="checkbox" checked={understood()} disabled={busy()} onChange={(e) => setUnderstood(e.currentTarget.checked)} />
            <span class="gv-check-label">{t("galleryInfo.purgeConfirm")}</span>
          </label>
        </Show>
        <Show when={error()}>
          {(message) => (
            <p class="gv-error" role="alert">
              {message()}
            </p>
          )}
        </Show>
      </div>
      <div class="gv-dialog-actions">
        <button type="button" id="gv-delete-cancel" class="btn" disabled={busy()} onClick={() => props.onCancel()}>
          {t("common.cancel")}
        </button>
        <button
          type="button"
          class="btn btn--danger"
          disabled={busy() || !count() || (!!props.purge && !understood())}
          onClick={() => void confirm()}
        >
          {busy() ? t("galleryInfo.deleting") : t(props.purge ? "galleryInfo.deleteForever" : "galleryInfo.moveToTrash")}
        </button>
      </div>
    </Modal>
  );
}
