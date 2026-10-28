import { createSignal, For, Show, onCleanup, onMount, type JSX } from "solid-js";
import { cleanLabelName, normLabel, type LabelInfo } from "../../api/keep";
import { t } from "../../i18n";
import { keep } from "../../state/keep";
import { AddIcon, CheckIcon, CloseIcon, EditIcon, LabelIcon, TrashIcon } from "./icons";

/** A small modal: title, body, Cancel and a confirm button. */
export function ConfirmDialog(props: { title: string; body: string; confirm: string; onConfirm: () => void; onCancel: () => void }) {
  onMount(() => {
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        props.onCancel();
      }
    };
    document.addEventListener("keydown", key, true);
    onCleanup(() => document.removeEventListener("keydown", key, true));
  });
  return (
    <div class="kp-scrim kp-scrim--front" onPointerDown={(e) => e.target === e.currentTarget && props.onCancel()}>
      <div class="kp-modal" role="alertdialog" aria-modal="true" aria-label={props.title}>
        <h2 class="kp-modal-title">{props.title}</h2>
        <p class="kp-modal-body">{props.body}</p>
        <div class="kp-modal-actions">
          <button type="button" class="kp-text-btn" onClick={props.onCancel}>
            {t("keep.cancel")}
          </button>
          <button type="button" class="kp-text-btn kp-text-btn--accent" data-action="confirm" onClick={props.onConfirm}>
            {props.confirm}
          </button>
        </div>
      </div>
    </div>
  );
}

function LabelRow(props: { label: LabelInfo; onDelete: (l: LabelInfo) => void }) {
  const [name, setName] = createSignal(props.label.name);
  const [focused, setFocused] = createSignal(false);
  const commit = () => {
    const next = cleanLabelName(name());
    setFocused(false);
    if (!next || next === props.label.name) return setName(props.label.name);
    void keep.renameLabel(props.label.tag, next);
  };
  return (
    <div class="kp-lrow" classList={{ "kp-lrow--focus": focused() }} data-label={props.label.tag}>
      <button type="button" class="kp-icon-btn kp-icon-btn--sm kp-lrow-icon" aria-label={t("keep.labelDelete")} title={t("keep.labelDelete")} onClick={() => props.onDelete(props.label)}>
        <span class="kp-lrow-label">
          <LabelIcon />
        </span>
        <span class="kp-lrow-trash">
          <TrashIcon />
        </span>
      </button>
      <input
        class="kp-lrow-input"
        value={name()}
        aria-label={t("keep.labelRename")}
        maxlength={50}
        onFocus={() => setFocused(true)}
        onInput={(e) => setName(e.currentTarget.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
          if (e.key === "Escape") {
            e.stopPropagation();
            setName(props.label.name);
            e.currentTarget.blur();
          }
        }}
      />
      <span class="kp-lrow-count">{props.label.count}</span>
      <span class="kp-lrow-edit" aria-hidden="true">
        {focused() ? <CheckIcon /> : <EditIcon />}
      </span>
    </div>
  );
}

/** "Edit labels": create, rename and delete labels. */
export function LabelsDialog(props: { onClose: () => void }): JSX.Element {
  const [text, setText] = createSignal("");
  const [error, setError] = createSignal("");
  const [pendingDelete, setPendingDelete] = createSignal<LabelInfo | null>(null);
  let input: HTMLInputElement | undefined;
  onMount(() => {
    input?.focus();
    const key = (e: KeyboardEvent) => e.key === "Escape" && !e.defaultPrevented && props.onClose();
    document.addEventListener("keydown", key);
    onCleanup(() => document.removeEventListener("keydown", key));
  });

  async function create() {
    const name = cleanLabelName(text());
    if (!normLabel(name)) return;
    if (keep.labels().some((l) => l.tag === normLabel(name))) return setError(t("keep.labelExists"));
    setError("");
    setText("");
    await keep.createLabel(name);
    input?.focus();
  }

  return (
    <div class="kp-scrim" onPointerDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div class="kp-modal kp-modal--labels" role="dialog" aria-modal="true" aria-label={t("keep.editLabelsTitle")}>
        <h2 class="kp-modal-title">{t("keep.editLabelsTitle")}</h2>
        <form
          class="kp-lrow kp-lrow--new"
          onSubmit={(e) => {
            e.preventDefault();
            void create();
          }}
        >
          <button type="button" class="kp-icon-btn kp-icon-btn--sm" aria-label={t("keep.cancel")} onClick={() => setText("")}>
            {text() ? <CloseIcon /> : <AddIcon />}
          </button>
          <input ref={input} class="kp-lrow-input" placeholder={t("keep.labelNew")} aria-label={t("keep.labelNew")} value={text()} maxlength={50} onInput={(e) => setText(e.currentTarget.value)} />
          <button type="submit" class="kp-icon-btn kp-icon-btn--sm" aria-label={t("keep.labelCreateBtn")} title={t("keep.labelCreateBtn")} disabled={!normLabel(text())}>
            <CheckIcon />
          </button>
        </form>
        <Show when={error()}>
          <p class="kp-modal-error" role="alert">
            {error()}
          </p>
        </Show>
        <div class="kp-lrows">
          <For each={keep.labels()}>{(l) => <LabelRow label={l} onDelete={setPendingDelete} />}</For>
        </div>
        <div class="kp-modal-actions">
          <button type="button" class="kp-text-btn" data-action="done" onClick={props.onClose}>
            {t("keep.done")}
          </button>
        </div>
      </div>
      <Show when={pendingDelete()}>
        {(l) => (
          <ConfirmDialog
            title={t("keep.labelDelete")}
            body={t("keep.labelDeleteBody")}
            confirm={t("keep.labelDelete")}
            onCancel={() => setPendingDelete(null)}
            onConfirm={() => {
              const tag = l().tag;
              setPendingDelete(null);
              void keep.deleteLabel(tag);
            }}
          />
        )}
      </Show>
    </div>
  );
}
