import { createMemo, createSignal, For, onMount, Show } from "solid-js";
import { newItemId, type ChecklistItem } from "../../api/keep";
import { t } from "../../i18n";
import { AddIcon, CheckBoxIcon, CheckboxOutlineIcon, CloseIcon, DragIcon, ExpandIcon } from "./icons";

/** Move `from` to just before `to` (or to the end when `to` is -1) in an array copy. */
export function moveItem<T>(arr: T[], from: number, to: number): T[] {
  if (from < 0 || from >= arr.length || from === to) return arr;
  const out = arr.slice();
  const [it] = out.splice(from, 1);
  const at = to < 0 ? out.length : from < to ? to - 1 : to;
  out.splice(at, 0, it as T);
  return out;
}

/** The note dialog's and composer's checklist: unchecked items, an add row, then a collapsible "Checked items". */
export function ChecklistEditor(props: { items: ChecklistItem[]; onChange: (items: ChecklistItem[]) => void; readOnly?: boolean; autofocus?: boolean }) {
  onMount(() => {
    if (props.autofocus) {
      const last = openIds()[openIds().length - 1];
      if (last) focusItem(last);
    }
  });
  const [checkedOpen, setCheckedOpen] = createSignal(true);
  const [dragId, setDragId] = createSignal<string | null>(null);
  const [overId, setOverId] = createSignal<string | null>(null);
  let root: HTMLDivElement | undefined;
  // Rows are keyed by id: typing replaces the item object, and the textarea must keep its focus.
  const sameIds = (a: string[], b: string[]) => a.length === b.length && a.every((x, i) => x === b[i]);
  const openIds = createMemo(() => props.items.filter((i) => !i.checked).map((i) => i.id), [], { equals: sameIds });
  const doneIds = createMemo(() => props.items.filter((i) => i.checked).map((i) => i.id), [], { equals: sameIds });

  const patch = (id: string, change: Partial<ChecklistItem>) => props.onChange(props.items.map((i) => (i.id === id ? { ...i, ...change } : i)));
  const remove = (id: string) => props.onChange(props.items.filter((i) => i.id !== id));
  const focusItem = (id: string, end = true) =>
    queueMicrotask(() => {
      const input = root?.querySelector<HTMLTextAreaElement>(`textarea[data-item="${id}"]`);
      input?.focus();
      if (input && end) input.setSelectionRange(input.value.length, input.value.length);
    });
  const grow = (el: HTMLTextAreaElement) => {
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  };

  function insertAfter(id: string | null, text = "") {
    const item: ChecklistItem = { id: newItemId(), text, checked: false, indent: id ? (props.items.find((i) => i.id === id)?.indent ?? 0) : 0 };
    const at = id ? props.items.findIndex((i) => i.id === id) + 1 : props.items.length;
    const next = props.items.slice();
    next.splice(at < 0 ? next.length : at, 0, item);
    props.onChange(next);
    focusItem(item.id);
  }

  function onKey(e: KeyboardEvent, item: ChecklistItem) {
    const input = e.currentTarget as HTMLTextAreaElement;
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      const at = input.selectionStart ?? input.value.length;
      if (at < input.value.length) {
        // Split the item at the caret, like Keep.
        patch(item.id, { text: input.value.slice(0, at) });
        insertAfter(item.id, input.value.slice(at));
      } else insertAfter(item.id);
    } else if (e.key === "Backspace" && input.value === "") {
      e.preventDefault();
      const idx = openIds().indexOf(item.id);
      remove(item.id);
      const prev = openIds()[idx - 1];
      if (prev) focusItem(prev);
    } else if (e.key === "Tab") {
      e.preventDefault();
      patch(item.id, { indent: e.shiftKey ? 0 : 1 });
    }
  }

  function drop(target: string) {
    const from = props.items.findIndex((i) => i.id === dragId());
    const to = props.items.findIndex((i) => i.id === target);
    setDragId(null);
    setOverId(null);
    if (from < 0 || to < 0 || from === to) return;
    props.onChange(moveItem(props.items, from, to));
  }

  const Row = (rowProps: { id: string }) => {
    const found = () => props.items.find((i) => i.id === rowProps.id);
    return (
      <Show when={found()}>
        {(item) => (
    <div
      class="kp-item"
      classList={{ "kp-item--done": item().checked, "kp-item--over": overId() === item().id && dragId() !== item().id, "kp-item--drag": dragId() === item().id }}
      data-indent={item().indent}
      onDragOver={(e) => {
        if (!dragId()) return;
        e.preventDefault();
        setOverId(item().id);
      }}
      onDrop={(e) => {
        e.preventDefault();
        drop(item().id);
      }}
    >
      <Show when={!props.readOnly}>
        <span
          class="kp-item-drag"
          draggable="true"
          title={t("keep.dragItem")}
          aria-label={t("keep.dragItem")}
          onDragStart={(e) => {
            setDragId(item().id);
            e.dataTransfer?.setData("text/plain", item().id);
            if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
          }}
          onDragEnd={() => {
            setDragId(null);
            setOverId(null);
          }}
        >
          <DragIcon />
        </span>
      </Show>
      <button
        type="button"
        class="kp-item-check"
        role="checkbox"
        aria-checked={item().checked}
        aria-label={item().text || t("keep.listItemPh")}
        disabled={props.readOnly}
        onClick={() => patch(item().id, { checked: !item().checked })}
      >
        {item().checked ? <CheckBoxIcon /> : <CheckboxOutlineIcon />}
      </button>
      <textarea
        class="kp-item-text"
        rows={1}
        data-item={item().id}
        value={item().text}
        placeholder={t("keep.listItemPh")}
        aria-label={t("keep.listItemPh")}
        readOnly={props.readOnly}
        ref={(el) => queueMicrotask(() => grow(el))}
        onInput={(e) => {
          grow(e.currentTarget);
          patch(item().id, { text: e.currentTarget.value });
        }}
        onKeyDown={(e) => onKey(e, item())}
      />
      <Show when={!props.readOnly}>
        <button type="button" class="kp-item-del kp-icon-btn kp-icon-btn--sm" aria-label={t("keep.deleteItem")} title={t("keep.deleteItem")} onClick={() => remove(item().id)}>
          <CloseIcon />
        </button>
      </Show>
    </div>
          )}
      </Show>
    );
  };

  return (
    <div class="kp-checklist" ref={root}>
      <For each={openIds()}>{(id) => <Row id={id} />}</For>
      <Show when={!props.readOnly}>
        <button type="button" class="kp-item-add" onClick={() => insertAfter(null)} >
          <AddIcon />
          <span>{t("keep.addListItem")}</span>
        </button>
      </Show>
      <Show when={doneIds().length > 0}>
        <button type="button" class="kp-checked-head" aria-expanded={checkedOpen()} onClick={() => setCheckedOpen(!checkedOpen())}>
          <span class="kp-checked-caret" classList={{ "kp-checked-caret--closed": !checkedOpen() }}>
            <ExpandIcon />
          </span>
          <span>{t("keep.checkedItems")}</span>
          <span class="kp-checked-count">{doneIds().length}</span>
        </button>
        <Show when={checkedOpen()}>
          <For each={doneIds()}>{(id) => <Row id={id} />}</For>
        </Show>
      </Show>
    </div>
  );
}
