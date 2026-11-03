import { createSignal, Show } from "solid-js";
import { patchPerson, type Person } from "../../../api/gallery";
import { isBirthDate } from "../../../lib/galleryPeople";
import { InlineName } from "./InlineName";
import { PersonFace } from "./PersonFace";
import "../../../styles/gallery-people.css";
import { groupText, useGroupKind } from "../../../lib/galleryGroup";

/** The top of a person's page: face, name (a click renames), birth date (optional) and how many photos. */
export function PersonHeader(props: { person: Person; onChanged: (person: Person) => void }) {
  const kind = useGroupKind();
  const pt = groupText(kind);
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
      props.onChanged(await patchPerson(props.person.id, { birthDate: value || null }, kind));
    } catch {
      setFailed(true);
    }
  }

  return (
    <div class="pp-person">
      <PersonFace src={props.person.thumbUrl} class="pp-face--large" />
      <div class="pp-person__text">
        <InlineName class="pp-name--large" name={props.person.name} label={pt("rename", { name: props.person.name || pt("unnamed") })} onSave={async (name) => props.onChanged(await patchPerson(props.person.id, { name }))} />
        <span class="pp-count">{pt("photos", { count: props.person.itemCount })}</span>
        <div class="pp-birth">
          <label for="pp-birth-date">{pt("birthDate")}</label>
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
              {pt("birthDateClear")}
            </button>
          </Show>
        </div>
        <Show when={invalid()}>
          <p class="gv-error" role="alert">
            {pt("birthDateInvalid")}
          </p>
        </Show>
        <Show when={failed()}>
          <p class="gv-error" role="alert">
            {pt("actionFailed")}
          </p>
        </Show>
      </div>
    </div>
  );
}
