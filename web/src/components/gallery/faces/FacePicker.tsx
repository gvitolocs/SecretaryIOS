import { createMemo, createResource, createSignal, For, Show } from "solid-js";
import { faceThumbUrl, listPeople, type FaceTarget, type Person } from "../../../api/gallery";
import { t } from "../../../i18n";
import { normalizeName, personName, searchPeople } from "../../../lib/galleryPeople";
import { Modal } from "../Modal";
import "../../../styles/gallery-people.css";
import { groupText, useGroupKind } from "../../../lib/galleryGroup";

export interface FacePickerProps {
  faceId: string;
  /** Who the face is now (null: nobody yet). */
  current: { id: string; name: string | null } | null;
  /** Saves the choice; rejects when the server refuses (the picker stays open with an error). */
  onAssign: (target: FaceTarget) => Promise<void>;
  onCancel: () => void;
}

/**
 * Who is this face? The people to choose from (searchable), a new person by name, or "Not this person" when the face has
 * one. A small dialog over the viewer or the People page; one request at a time.
 */
export function FacePicker(props: FacePickerProps) {
  const kind = useGroupKind();
  const pt = groupText(kind);
  const [people] = createResource(() => listPeople({ minFaces: 1 }, kind));
  const [query, setQuery] = createSignal("");
  const [name, setName] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  // Named people first, then the unnamed ones (found by clustering); the search only matches names.
  const shown = createMemo(() => {
    const all = people.error ? [] : (people() ?? []);
    const found = searchPeople(all, query());
    return found.slice().sort((a, b) => Number(!!b.name) - Number(!!a.name));
  });

  const choose = async (target: FaceTarget) => {
    if (busy()) return;
    setBusy(true);
    setError(null);
    try {
      await props.onAssign(target);
    } catch {
      setError(pt("assignFailed"));
      setBusy(false);
    }
  };

  const create = (e: SubmitEvent) => {
    e.preventDefault();
    const typed = normalizeName(name());
    if (typed) void choose({ newPerson: typed });
  };

  return (
    <Modal labelledBy="gf-pick-title" busy={busy()} onClose={props.onCancel}>
      <h2 id="gf-pick-title" class="gv-dialog-title">
        {pt("pickerTitle")}
      </h2>
      <div class="gf-pick-head">
        <img class="gf-pick-face" src={faceThumbUrl(props.faceId, kind)} alt="" draggable={false} />
        <Show when={props.current}>
          {(now) => <p class="gv-dialog-sub">{pt("pickerNow", { name: personName(now(), pt("unnamed")) })}</p>}
        </Show>
      </div>
      <div class="gv-dialog-body">
        <input
          class="text-input"
          type="search"
          value={query()}
          placeholder={pt("pickerSearch")}
          aria-label={pt("pickerSearch")}
          disabled={busy()}
          onInput={(e) => setQuery(e.currentTarget.value)}
        />
        <Show when={!people.loading} fallback={<p class="gv-muted" role="status">{pt("pickerLoading")}</p>}>
          <Show when={!people.error} fallback={<p class="gv-error" role="alert">{pt("pickerError")}</p>}>
            <Show when={shown().length} fallback={<p class="gv-muted">{pt("pickerNone")}</p>}>
              <ul class="gf-people" aria-label={pt("title")}>
                <For each={shown()}>{(person) => <PickRow person={person} current={props.current?.id === person.id} busy={busy()} onPick={() => void choose({ personId: person.id })} />}</For>
              </ul>
            </Show>
          </Show>
        </Show>
        <form class="gf-new" onSubmit={create}>
          <label class="field-label" for="gf-new-person">
            {pt("newPerson")}
          </label>
          <div class="gf-new__row">
            <input
              id="gf-new-person"
              class="text-input"
              type="text"
              maxLength={80}
              placeholder={pt("newPersonName")}
              value={name()}
              disabled={busy()}
              onInput={(e) => setName(e.currentTarget.value)}
            />
            <button type="submit" class="btn btn--primary" disabled={busy() || !normalizeName(name())}>
              {pt("createPerson")}
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
        <Show when={props.current}>
          <button type="button" class="btn gf-not" disabled={busy()} onClick={() => void choose({ personId: null })}>
            {pt("notThisPerson")}
          </button>
        </Show>
        <button type="button" class="btn" disabled={busy()} onClick={() => props.onCancel()}>
          {t("common.cancel")}
        </button>
      </div>
    </Modal>
  );
}

function PickRow(props: { person: Person; current: boolean; busy: boolean; onPick: () => void }) {
  const kind = useGroupKind();
  const pt = groupText(kind);
  return (
    <li>
      <button type="button" class="gf-person" aria-current={props.current ? "true" : undefined} disabled={props.busy || props.current} onClick={props.onPick}>
        <Show when={props.person.thumbUrl} fallback={<span class="gf-person__img gf-person__img--empty" aria-hidden="true" />}>
          {(src) => <img class="gf-person__img" src={src()} alt="" loading="lazy" draggable={false} />}
        </Show>
        <span class="gf-person__name">{personName(props.person, pt("unnamed"))}</span>
        <span class="gf-person__count">{pt("photos", { count: props.person.itemCount })}</span>
      </button>
    </li>
  );
}
