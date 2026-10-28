import { createSignal, Show } from "solid-js";
import { createAlbum, deleteAlbum, renameAlbum, type AlbumCard } from "../../../api/gallery";
import { t } from "../../../i18n";
import { Modal } from "../Modal";

/** Runs `work`, keeping the dialog busy meanwhile, and shows one error line when it throws. */
function useAction() {
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const run = async (work: () => Promise<void>) => {
    if (busy()) return;
    setBusy(true);
    setError(null);
    try {
      await work();
    } catch {
      setError(t("galleryAlbums.actionFailed"));
      setBusy(false);
    }
  };
  return { busy, error, run };
}

function Errors(props: { message: string | null }) {
  return (
    <Show when={props.message}>
      {(message) => (
        <p class="gv-error" role="alert">
          {message()}
        </p>
      )}
    </Show>
  );
}

/** A name for an album: used to make one and to rename one. */
function NameDialog(props: { id: string; heading: string; submit: string; initial: string; onSave: (title: string) => Promise<void>; onCancel: () => void }) {
  const [title, setTitle] = createSignal(props.initial);
  const { busy, error, run } = useAction();
  return (
    <Modal labelledBy={`${props.id}-title`} busy={busy()} onClose={props.onCancel}>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          const name = title().trim();
          if (name) void run(() => props.onSave(name));
        }}
      >
        <h2 id={`${props.id}-title`} class="gv-dialog-title">
          {props.heading}
        </h2>
        <div class="gv-dialog-body">
          <label class="field-label" for={`${props.id}-name`}>
            {t("galleryAlbums.nameLabel")}
          </label>
          <input
            id={`${props.id}-name`}
            class="text-input"
            type="text"
            maxLength={120}
            placeholder={t("galleryAlbums.namePlaceholder")}
            value={title()}
            disabled={busy()}
            onInput={(e) => setTitle(e.currentTarget.value)}
          />
          <Errors message={error()} />
        </div>
        <div class="gv-dialog-actions">
          <button type="button" class="btn" disabled={busy()} onClick={() => props.onCancel()}>
            {t("common.cancel")}
          </button>
          <button type="submit" class="btn btn--primary" disabled={busy() || !title().trim()}>
            {busy() ? t("common.saving") : props.submit}
          </button>
        </div>
      </form>
    </Modal>
  );
}

export function NewAlbumDialog(props: { onCreated: (album: { id: string; title: string }) => void; onCancel: () => void }) {
  return (
    <NameDialog
      id="al-new"
      heading={t("galleryAlbums.newTitle")}
      submit={t("galleryAlbums.create")}
      initial=""
      onSave={async (title) => props.onCreated(await createAlbum(title))}
      onCancel={props.onCancel}
    />
  );
}

export function RenameAlbumDialog(props: { album: AlbumCard; onRenamed: (album: AlbumCard) => void; onCancel: () => void }) {
  return (
    <NameDialog
      id="al-rename"
      heading={t("galleryAlbums.renameTitle")}
      submit={t("common.save")}
      initial={props.album.title}
      onSave={async (title) => props.onRenamed(await renameAlbum(props.album.id, title))}
      onCancel={props.onCancel}
    />
  );
}

/** Deleting an album never deletes its photos. */
export function DeleteAlbumDialog(props: { album: AlbumCard; onDeleted: () => void; onCancel: () => void }) {
  const { busy, error, run } = useAction();
  return (
    <Modal labelledBy="al-delete-title" busy={busy()} onClose={props.onCancel}>
      <h2 id="al-delete-title" class="gv-dialog-title">
        {t("galleryAlbums.deleteTitle", { title: props.album.title || t("galleryAlbums.untitled") })}
      </h2>
      <div class="gv-dialog-body">
        <p>{t("galleryAlbums.deleteBody")}</p>
        <Errors message={error()} />
      </div>
      <div class="gv-dialog-actions">
        <button type="button" class="btn" disabled={busy()} onClick={() => props.onCancel()}>
          {t("common.cancel")}
        </button>
        <button
          type="button"
          class="btn btn--danger"
          disabled={busy()}
          onClick={() =>
            void run(async () => {
              await deleteAlbum(props.album.id);
              props.onDeleted();
            })
          }
        >
          {t("galleryAlbums.deleteConfirm")}
        </button>
      </div>
    </Modal>
  );
}
