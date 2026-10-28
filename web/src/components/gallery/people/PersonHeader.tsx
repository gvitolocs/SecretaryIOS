import { createSignal, Show } from "solid-js";
import { patchPerson, type Person } from "../../../api/gallery";
import { t } from "../../../i18n";
import { isBirthDate } from "../../../lib/galleryPeople";
import { InlineName } from "./InlineName";
import { PersonFace } from "./PersonFace";
import "../../../styles/gallery-people.css";

/** The top of a person's page: face, name (a click renames), birth date (optional) and how many photos. */
export function PersonHeader(props: { person: Person; onChanged: (person: Person) => void }) {
  const [invalid, setInvalid] = createSignal(false);
  const [failed, setFailed] = createSignal(false);

  async function saveBirth(value: string): Promise<void> {
    setFailed(false);
    if (value && !isBirthDate(value)) {
      setInvalid(true);
      return;
    }
    setInvalid(false);
    if ((value || null) === props.person.birthDate) return;
    try {
      props.onChanged(await patchPerson(props.person.id, { birthDate: value || null }));
    } catch {
      setFailed(true);
    }
  }

  return (
    <div class="pp-person">
      <PersonFace src={props.person.thumbUrl} class="pp-face--large" />
      <div class="pp-person__text">
        <InlineName class="pp-name--large" name={props.person.name} label={t("galleryPeople.rename", { name: props.person.name || t("galleryPeople.unnamed") })} onSave={async (name) => props.onChanged(await patchPerson(props.person.id, { name }))} />
        <span class="pp-count">{t("galleryPeople.photos", { count: props.person.itemCount })}</span>
        <div class="pp-birth">
          <label for="pp-birth-date">{t("galleryPeople.birthDate")}</label>
          <input
            id="pp-birth-date"
            class="text-input"
            type="date"
            max={new Date().toISOString().slice(0, 10)}
            value={props.person.birthDate ?? ""}
            aria-invalid={invalid() ? "true" : undefined}
            onChange={(e) => void saveBirth(e.currentTarget.value)}
          />
          <Show when={props.person.birthDate}>
            <button type="button" class="g-chip" onClick={() => void saveBirth("")}>
              {t("galleryPeople.birthDateClear")}
            </button>
          </Show>
        </div>
        <Show when={invalid()}>
          <p class="gv-error" role="alert">
            {t("galleryPeople.birthDateInvalid")}
          </p>
        </Show>
        <Show when={failed()}>
          <p class="gv-error" role="alert">
            {t("galleryPeople.actionFailed")}
          </p>
        </Show>
      </div>
    </div>
  );
}
