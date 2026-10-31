import { createEffect, createSignal, For, on, Show } from "solid-js";
import type { Routine } from "../../api/gym";
import { gym, runGym } from "../../state/gym";
import { ChevronDownIcon, ChevronUpIcon, PlusCircleIcon } from "./icons";
import ExercisePicker from "./Picker";
import { gt } from "./strings";

/** The engine's routine rule: 1–30 distinct catalog exercises. */
export const MAX_ROUTINE_EXERCISES = 30;

/** Move the item at `from` to `to` (both clamped), returning a new array. */
export function moveItem<T>(items: readonly T[], from: number, to: number): T[] {
  const next = [...items];
  if (from < 0 || from >= next.length) return next;
  const target = Math.max(0, Math.min(next.length - 1, to));
  const [item] = next.splice(from, 1);
  next.splice(target, 0, item as T);
  return next;
}

/**
 * New routine (ExercisePicker.swift `RoutineBuilder`): a name, catalog
 * exercises in order, then `save_routine` with `exercise_ids`. The engine
 * gives each exercise three sets of eight. With `routine` it edits that
 * routine instead (`update_routine`: the name and the exercises in order;
 * exercises it already had keep their sets), focused on the name or on the
 * exercise list (`focus`).
 */
export default function RoutineBuilder(props: {
  open: boolean;
  onClose: () => void;
  routine?: Routine;
  focus?: "name" | "exercises";
}) {
  const [name, setName] = createSignal("");
  const [selected, setSelected] = createSignal<string[]>([]);
  const [pickerOpen, setPickerOpen] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [saving, setSaving] = createSignal(false);
  const [dragFrom, setDragFrom] = createSignal<number | null>(null);
  let nameInput: HTMLInputElement | undefined;
  let addButton: HTMLButtonElement | undefined;

  const nameOf = (id: string) => gym.view?.state.exercises.find((e) => e.id === id)?.name ?? id;

  createEffect(
    on(
      () => props.open,
      (open) => {
        if (!open) return;
        setName(props.routine?.name ?? "");
        setSelected(props.routine?.exercises.map((e) => e.exercise_id) ?? []);
        setError(null);
        setPickerOpen(false);
        queueMicrotask(() => (props.focus === "exercises" ? addButton : nameInput)?.focus());
      },
    ),
  );

  function move(from: number, to: number) {
    setSelected((ids) => moveItem(ids, from, to));
  }

  async function save(e: SubmitEvent) {
    e.preventDefault();
    if (saving()) return;
    const routineName = name().trim();
    const ids = selected();
    if (!routineName) {
      setError(gt("nameRequired"));
      return;
    }
    if (ids.length === 0) {
      setError(gt("exercisesRequired"));
      return;
    }
    setError(null);
    setSaving(true);
    try {
      const editing = props.routine;
      const res = await runGym(
        editing
          ? { type: "update_routine", routine_id: editing.id, name: routineName, exercise_ids: ids }
          : { type: "save_routine", name: routineName, exercise_ids: ids },
      );
      if (res?.ok) props.onClose();
    } finally {
      setSaving(false);
    }
  }

  return (
    <Show when={props.open}>
      <div class="picker-overlay" onClick={(e) => e.target === e.currentTarget && props.onClose()}>
        <div
          class="picker-sheet routine-builder"
          role="dialog"
          aria-modal="true"
          aria-labelledby="routine-builder-title"
          onKeyDown={(e) => {
            if (e.key === "Escape" && !pickerOpen()) {
              e.stopPropagation();
              props.onClose();
            }
          }}
        >
          <header class="picker-head">
            <h2 id="routine-builder-title" class="section-title">
              {props.routine ? gt("editRoutine") : gt("newRoutine")}
            </h2>
            <button type="button" class="icon-btn" aria-label={gt("close")} onClick={() => props.onClose()}>
              ×
            </button>
          </header>
          <form class="routine-form" onSubmit={save}>
            <p class="gym-eyebrow">{gt("routine")}</p>
            <label class="sr-only" for="routine-builder-name">
              {gt("routineName")}
            </label>
            <input
              id="routine-builder-name"
              ref={nameInput}
              class="text-input"
              maxlength="120"
              placeholder={gt("namePlaceholder")}
              autocomplete="off"
              value={name()}
              onInput={(e) => setName(e.currentTarget.value)}
            />

            <p class="gym-eyebrow">{gt("exercisesInOrder")}</p>
            <Show
              when={selected().length > 0}
              fallback={<p class="muted small">{gt("noExercisesYet")}</p>}
            >
              <ol class="routine-list" aria-label={gt("exercisesInOrder")}>
                <For each={selected()}>
                  {(id, i) => (
                    <li
                      class="routine-item"
                      classList={{ "routine-item--dragging": dragFrom() === i() }}
                      draggable="true"
                      onDragStart={(e) => {
                        setDragFrom(i());
                        e.dataTransfer?.setData("text/plain", id);
                      }}
                      onDragOver={(e) => e.preventDefault()}
                      onDrop={(e) => {
                        e.preventDefault();
                        const from = dragFrom();
                        if (from !== null) move(from, i());
                        setDragFrom(null);
                      }}
                      onDragEnd={() => setDragFrom(null)}
                    >
                      <span class="routine-item-index" aria-hidden="true">
                        {i() + 1}
                      </span>
                      <span class="routine-item-name">{nameOf(id)}</span>
                      <span class="routine-item-actions">
                        <button
                          type="button"
                          class="icon-btn"
                          aria-label={gt("moveUp", { name: nameOf(id) })}
                          disabled={i() === 0}
                          onClick={() => move(i(), i() - 1)}
                        >
                          <ChevronUpIcon />
                        </button>
                        <button
                          type="button"
                          class="icon-btn"
                          aria-label={gt("moveDown", { name: nameOf(id) })}
                          disabled={i() === selected().length - 1}
                          onClick={() => move(i(), i() + 1)}
                        >
                          <ChevronDownIcon />
                        </button>
                        <button
                          type="button"
                          class="icon-btn"
                          aria-label={gt("removeNamed", { name: nameOf(id) })}
                          onClick={() => setSelected((ids) => ids.filter((x) => x !== id))}
                        >
                          ×
                        </button>
                      </span>
                    </li>
                  )}
                </For>
              </ol>
            </Show>
            <button
              type="button"
              class="btn btn--block routine-add"
              ref={addButton}
              disabled={selected().length >= MAX_ROUTINE_EXERCISES}
              onClick={() => setPickerOpen(true)}
            >
              <PlusCircleIcon /> {gt("addExercise")}
            </button>
            <p class="muted small">{props.routine ? gt("editNote") : gt("builderNote")}</p>
            <Show when={error()}>
              {(msg) => (
                <p class="field-error" role="alert">
                  {msg()}
                </p>
              )}
            </Show>
            <div class="picker-create-actions">
              <button type="submit" class="btn btn--primary" disabled={saving()}>
                {gt("saveRoutine")}
              </button>
              <button type="button" class="btn" onClick={() => props.onClose()}>
                {gt("cancel")}
              </button>
            </div>
          </form>
        </div>
      </div>
      <ExercisePicker
        open={pickerOpen()}
        title={gt("exercises")}
        activeExerciseIds={selected()}
        onSelect={(exercise) => {
          setSelected((ids) => (ids.includes(exercise.id) ? ids : [...ids, exercise.id]));
          setPickerOpen(false);
          addButton?.focus();
        }}
        onClose={() => {
          setPickerOpen(false);
          addButton?.focus();
        }}
      />
    </Show>
  );
}
