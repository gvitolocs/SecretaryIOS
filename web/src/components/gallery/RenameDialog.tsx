import { createSignal, Show } from "solid-js";
import type { GalleryItem } from "../../api/gallery";
import { t } from "../../i18n";
import { Modal } from "./Modal";

export interface RenameDialogProps {
  item: GalleryItem;
  /** Saves the new name and caption; throw to keep the dialog open with an error. */
  onSave: (edit: { title: string; caption: string }) => Promise<void> | void;
  onCancel: () => void;
}

/** Rename a picture (its title) and edit its caption. */
export function RenameDialog(props: RenameDialogProps) {
  const [title, setTitle] = createSignal(props.item.title);
  const [caption, setCaption] = createSignal(props.item.caption ?? "");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    if (busy() || !title().trim()) return;
    setBusy(true);
    setError(null);
    try {
      await props.onSave({ title: title().trim(), caption: caption().trim() });
    } catch {
      setError(t("galleryInfo.actionFailed"));
      setBusy(false);
    }
  };

  return (
    <Modal labelledBy="gv-rename-title" busy={busy()} onClose={props.onCancel}>
      <form onSubmit={(e) => void submit(e)}>
        <h2 id="gv-rename-title" class="gv-dialog-title">
          {t("galleryInfo.renameTitle")}
        </h2>
        <div class="gv-dialog-body">
          <label class="field-label" for="gv-rename-name">
            {t("galleryInfo.nameLabel")}
          </label>
          <input
            id="gv-rename-name"
            class="text-input"
            type="text"
            maxLength={200}
            value={title()}
            disabled={busy()}
            onInput={(e) => setTitle(e.currentTarget.value)}
          />
          <label class="field-label gv-gap" for="gv-rename-caption">
            {t("galleryInfo.captionLabel")}
          </label>
          <textarea
            id="gv-rename-caption"
            class="text-input gv-textarea"
            rows={3}
            maxLength={2000}
            placeholder={t("galleryInfo.captionPlaceholder")}
            value={caption()}
            disabled={busy()}
            onInput={(e) => setCaption(e.currentTarget.value)}
          />
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
          <button type="submit" class="btn btn--primary" disabled={busy() || !title().trim()}>
            {busy() ? t("common.saving") : t("common.save")}
          </button>
        </div>
      </form>
    </Modal>
  );
}
