import { createSignal, Show } from "solid-js";
import { t } from "../../../i18n";
import "../../../styles/gallery-people.css";

/**
 * Shown while face recognition is off: what it does, that it stays on this server, that it is the owner's to switch on,
 * and (when it found something earlier) a way to delete that.
 */
export function FacesOffCard(props: { hasData: boolean; onTurnOn: () => Promise<void>; onDelete: () => void }) {
  const [busy, setBusy] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  async function turnOn() {
    if (busy()) return;
    setBusy(true);
    setFailed(false);
    try {
      await props.onTurnOn();
    } catch {
      setFailed(true);
      setBusy(false);
    }
  }
  return (
    <section class="pp-card" aria-labelledby="pp-off-title">
      <h2 id="pp-off-title" class="pp-card__title">
        {t("galleryPeople.offTitle")}
      </h2>
      <p>{t("galleryPeople.offBody")}</p>
      <p class="pp-card__privacy">{t("galleryPeople.offPrivacy")}</p>
      <p class="pp-card__soft">{t("galleryPeople.offControl")}</p>
      <div class="pp-card__actions">
        <button type="button" class="btn btn--primary" disabled={busy()} onClick={() => void turnOn()}>
          {busy() ? t("galleryPeople.turningOn") : t("galleryPeople.turnOn")}
        </button>
        <Show when={props.hasData}>
          <button type="button" class="btn" disabled={busy()} onClick={props.onDelete}>
            {t("galleryPeople.deleteData")}
          </button>
        </Show>
      </div>
      <Show when={failed()}>
        <p class="gv-error" role="alert">
          {t("galleryPeople.actionFailed")}
        </p>
      </Show>
    </section>
  );
}
