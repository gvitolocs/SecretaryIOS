import { createSignal, onMount, Show } from "solid-js";
import { PERSON_NAME_MAX } from "../../../api/gallery";
import { t } from "../../../i18n";
import { normalizeName } from "../../../lib/galleryPeople";
import "../../../styles/gallery-people.css";

export interface InlineNameProps {
  /** The name now (null: none yet, the button says "Add a name"). */
  name: string | null;
  /** Saves the new name (null removes it); rejects when the server refuses. */
  onSave: (name: string | null) => Promise<void>;
  /** The text on the button when there is no name. */
  empty?: string;
  class?: string;
  /** Extra text for screen readers on the button (what is being renamed). */
  label?: string;
}

/** A name that is a button; a click turns it into a field. Enter saves, Escape or a click away leaves it as it was. */
export function InlineName(props: InlineNameProps) {
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [failed, setFailed] = createSignal(false);

  const open = () => {
    setDraft(props.name ?? "");
    setFailed(false);
    setEditing(true);
  };

  async function save(): Promise<void> {
    if (busy()) return;
    const next = normalizeName(draft());
    if (next === (props.name?.trim() || null)) {
      setEditing(false);
      return;
    }
    setBusy(true);
    setFailed(false);
    try {
      await props.onSave(next);
      setEditing(false);
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Show
      when={editing()}
      fallback={
        <button type="button" class={`pp-name ${props.class ?? ""}`} classList={{ "pp-name--empty": !props.name }} aria-label={props.label} onClick={open}>
          {props.name || props.empty || t("galleryPeople.addName")}
        </button>
      }
    >
      <form
        class={`pp-rename ${props.class ?? ""}`}
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <input
          class="text-input pp-rename__input"
          type="text"
          maxLength={PERSON_NAME_MAX}
          value={draft()}
          disabled={busy()}
          aria-label={t("galleryPeople.renameLabel")}
          aria-invalid={failed() ? "true" : undefined}
          placeholder={t("galleryPeople.renamePlaceholder")}
          ref={(el) => onMount(() => el.focus())}
          onInput={(e) => setDraft(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              e.stopPropagation();
              if (!busy()) setEditing(false);
            }
          }}
          onBlur={() => {
            // Leaving the field without Enter keeps the old name, except when the field is untouched.
            if (!busy() && !failed()) setEditing(false);
          }}
        />
        <Show when={failed()}>
          <span class="pp-rename__error" role="alert">
            {t("galleryPeople.actionFailed")}
          </span>
        </Show>
      </form>
    </Show>
  );
}
