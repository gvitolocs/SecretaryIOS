import { createSignal, For, Show } from "solid-js";
import { deleteFaceData, mergePeople, type Person } from "../../../api/gallery";
import { t } from "../../../i18n";
import { defaultKeeper, personName } from "../../../lib/galleryPeople";
import { Modal } from "../Modal";
import { PersonFace } from "./PersonFace";

/** Runs `work`, keeping the dialog busy meanwhile, and shows one error line when it throws. */
function useAction() {
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal(false);
  const run = async (work: () => Promise<void>) => {
    if (busy()) return;
    setBusy(true);
    setError(false);
    try {
      await work();
    } catch {
      setError(true);
      setBusy(false);
    }
  };
  return { busy, error, run };
}

function ErrorLine(props: { show: boolean }) {
  return (
    <Show when={props.show}>
      <p class="gv-error" role="alert">
        {t("galleryPeople.actionFailed")}
      </p>
    </Show>
  );
}

/** Pick the person to keep among the chosen ones; the others' faces move to them and the others go. */
export function MergeDialog(props: { people: Person[]; onMerged: (keep: Person, from: string[]) => void; onCancel: () => void }) {
  const [keep, setKeep] = createSignal(defaultKeeper(props.people) ?? "");
  const { busy, error, run } = useAction();
  const submit = (e: SubmitEvent) => {
    e.preventDefault();
    const id = keep();
    const others = props.people.filter((p) => p.id !== id).map((p) => p.id);
    if (!id || !others.length) return;
    void run(async () => props.onMerged(await mergePeople(id, others), others));
  };
  return (
    <Modal labelledBy="pp-merge-title" busy={busy()} onClose={props.onCancel}>
      <form onSubmit={submit}>
        <h2 id="pp-merge-title" class="gv-dialog-title">
          {t("galleryPeople.mergeTitle")}
        </h2>
        <div class="gv-dialog-body">
          <p>{t("galleryPeople.mergeBody")}</p>
          <ul class="pp-keep" role="radiogroup" aria-label={t("galleryPeople.mergeKeep")}>
            <For each={props.people}>
              {(person) => (
                <li>
                  <label class="pp-keep__row" classList={{ "pp-keep__row--on": keep() === person.id }}>
                    <input type="radio" name="pp-keep" value={person.id} checked={keep() === person.id} disabled={busy()} onChange={() => setKeep(person.id)} />
                    <PersonFace src={person.thumbUrl} class="pp-face--small" />
                    <span class="pp-keep__name">
                      {t("galleryPeople.mergeKeep")} {personName(person, t("galleryPeople.unnamed"))}
                    </span>
                    <span class="pp-count">{t("galleryPeople.photos", { count: person.itemCount })}</span>
                  </label>
                </li>
              )}
            </For>
          </ul>
          <ErrorLine show={error()} />
        </div>
        <div class="gv-dialog-actions">
          <button type="button" class="btn" disabled={busy()} onClick={() => props.onCancel()}>
            {t("common.cancel")}
          </button>
          <button type="submit" class="btn btn--primary" disabled={busy() || props.people.length < 2}>
            {busy() ? t("common.saving") : t("galleryPeople.mergeConfirm", { count: props.people.length })}
          </button>
        </div>
      </form>
    </Modal>
  );
}

/** Deleting face data removes faces, people, names and crops; the photos are never touched. */
export function DeleteFaceDataDialog(props: { onDeleted: () => void; onCancel: () => void }) {
  const { busy, error, run } = useAction();
  return (
    <Modal labelledBy="pp-delete-title" busy={busy()} onClose={props.onCancel}>
      <h2 id="pp-delete-title" class="gv-dialog-title">
        {t("galleryPeople.deleteTitle")}
      </h2>
      <div class="gv-dialog-body">
        <p>{t("galleryPeople.deleteBody")}</p>
        <ErrorLine show={error()} />
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
              await deleteFaceData();
              props.onDeleted();
            })
          }
        >
          {t("galleryPeople.deleteConfirm")}
        </button>
      </div>
    </Modal>
  );
}
