import { createEffect, createMemo, createSignal, For, on, Show } from "solid-js";
import type { Exercise, ExerciseKind } from "../../api/gym";
import { gym, runGym } from "../../state/gym";
import { EXERCISE_KINDS, kindLabel } from "./shared";
import { gt } from "./strings";

function distinctSorted(values: string[]): string[] {
  return [...new Set(values)].sort((a, b) => a.localeCompare(b));
}

function Chips(props: { label: string; values: string[]; selected: string | null; onSelect: (v: string | null) => void }) {
  return (
    <div class="chip-group" role="group" aria-label={props.label}>
      <For each={props.values}>
        {(value) => (
          <button
            type="button"
            class="chip"
            classList={{ "chip--active": props.selected === value }}
            aria-pressed={props.selected === value}
            onClick={() => props.onSelect(props.selected === value ? null : value)}
          >
            {value}
          </button>
        )}
      </For>
    </div>
  );
}

function CreateExerciseForm(props: { muscles: string[]; equipment: string[]; onDone: (name: string | null) => void }) {
  const [name, setName] = createSignal("");
  const [muscle, setMuscle] = createSignal("");
  const [equipment, setEquipment] = createSignal("");
  const [kind, setKind] = createSignal<ExerciseKind>("weight_reps");
  const [saving, setSaving] = createSignal(false);

  async function save(e: SubmitEvent) {
    e.preventDefault();
    if (saving()) return;
    setSaving(true);
    const created = name().trim();
    try {
      const res = await runGym({
        type: "create_exercise",
        name: created,
        muscle: muscle(),
        equipment: equipment(),
        kind: kind(),
      });
      if (res?.ok) props.onDone(created);
    } finally {
      setSaving(false);
    }
  }

  return (
    <form class="picker-create" onSubmit={save} aria-label={gt("newExercise")}>
      <label class="field-label" for="new-ex-name">
        {gt("exerciseName")}
      </label>
      <input
        id="new-ex-name"
        class="text-input"
        maxlength="120"
        value={name()}
        onInput={(e) => setName(e.currentTarget.value)}
      />
      <label class="field-label" for="new-ex-muscle">
        {gt("muscleGroup")}
      </label>
      <input
        id="new-ex-muscle"
        class="text-input"
        list="new-ex-muscles"
        maxlength="120"
        value={muscle()}
        onInput={(e) => setMuscle(e.currentTarget.value)}
      />
      <datalist id="new-ex-muscles">
        <For each={props.muscles}>{(m) => <option value={m} />}</For>
      </datalist>
      <label class="field-label" for="new-ex-equipment">
        {gt("equipment")}
      </label>
      <input
        id="new-ex-equipment"
        class="text-input"
        list="new-ex-equipments"
        maxlength="120"
        value={equipment()}
        onInput={(e) => setEquipment(e.currentTarget.value)}
      />
      <datalist id="new-ex-equipments">
        <For each={props.equipment}>{(m) => <option value={m} />}</For>
      </datalist>
      <label class="field-label" for="new-ex-kind">
        {gt("tracking")}
      </label>
      <select
        id="new-ex-kind"
        class="text-input"
        value={kind()}
        onChange={(e) => setKind(e.currentTarget.value as ExerciseKind)}
      >
        <For each={EXERCISE_KINDS}>{(k) => <option value={k}>{kindLabel(k)}</option>}</For>
      </select>
      <div class="picker-create-actions">
        <button type="submit" class="btn btn--primary" disabled={saving()}>
          {gt("saveExercise")}
        </button>
        <button type="button" class="btn" onClick={() => props.onDone(null)}>
          {gt("cancel")}
        </button>
      </div>
    </form>
  );
}

/**
 * Catalog picker: client-side filtering of the server's exercise list.
 * Without `onSelect` a choice adds the exercise to the active workout; with
 * it (the routine builder) the choice is handed back and nothing is sent.
 */
export default function ExercisePicker(props: {
  open: boolean;
  activeExerciseIds: string[];
  onClose: () => void;
  onSelect?: (exercise: Exercise) => void;
  title?: string;
}) {
  const [query, setQuery] = createSignal("");
  const [muscle, setMuscle] = createSignal<string | null>(null);
  const [equipment, setEquipment] = createSignal<string | null>(null);
  const [creating, setCreating] = createSignal(false);
  const [adding, setAdding] = createSignal<string | null>(null);
  let searchInput: HTMLInputElement | undefined;

  const catalog = (): Exercise[] => gym.view?.state.exercises ?? [];
  const muscles = createMemo(() => distinctSorted(catalog().map((e) => e.muscle)));
  const equipments = createMemo(() => distinctSorted(catalog().map((e) => e.equipment)));
  const added = createMemo(() => new Set(props.activeExerciseIds));

  // Same match rule as the engine's search_exercises: substring of "name muscle equipment".
  const results = createMemo(() => {
    const q = query().trim().toLowerCase();
    const m = muscle();
    const eq = equipment();
    return catalog().filter(
      (e) =>
        (!q || `${e.name} ${e.muscle} ${e.equipment}`.toLowerCase().includes(q)) &&
        (!m || e.muscle === m) &&
        (!eq || e.equipment === eq),
    );
  });

  createEffect(
    on(
      () => props.open,
      (open) => {
        if (!open) return;
        setCreating(false);
        queueMicrotask(() => searchInput?.focus());
      },
    ),
  );

  async function choose(exercise: Exercise) {
    if (adding() || added().has(exercise.id)) return;
    if (props.onSelect) {
      props.onSelect(exercise);
      return;
    }
    setAdding(exercise.id);
    try {
      const res = await runGym({ type: "add_exercise", exercise_id: exercise.id });
      if (res?.ok) props.onClose();
    } finally {
      setAdding(null);
    }
  }

  return (
    <Show when={props.open}>
      <div class="picker-overlay" onClick={(e) => e.target === e.currentTarget && props.onClose()}>
        <div
          class="picker-sheet"
          role="dialog"
          aria-modal="true"
          aria-labelledby="picker-title"
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.stopPropagation();
              props.onClose();
            }
          }}
        >
          <header class="picker-head">
            <h2 id="picker-title" class="section-title">
              {creating() ? gt("newExercise") : (props.title ?? gt("addExercise"))}
            </h2>
            <button type="button" class="icon-btn" aria-label={gt("close")} onClick={() => props.onClose()}>
              ×
            </button>
          </header>
          <Show
            when={!creating()}
            fallback={
              <CreateExerciseForm
                muscles={muscles()}
                equipment={equipments()}
                onDone={(name) => {
                  setCreating(false);
                  if (name) {
                    setMuscle(null);
                    setEquipment(null);
                    setQuery(name);
                  }
                  queueMicrotask(() => searchInput?.focus());
                }}
              />
            }
          >
            <label class="sr-only" for="picker-search">
              {gt("searchExercises")}
            </label>
            <input
              id="picker-search"
              ref={searchInput}
              class="text-input"
              type="search"
              placeholder={gt("searchPlaceholder")}
              autocomplete="off"
              value={query()}
              onInput={(e) => setQuery(e.currentTarget.value)}
            />
            <Chips label={gt("muscle")} values={muscles()} selected={muscle()} onSelect={setMuscle} />
            <Chips label={gt("equipment")} values={equipments()} selected={equipment()} onSelect={setEquipment} />
            <Show
              when={results().length > 0}
              fallback={<p class="muted center small">{gt("noMatches")}</p>}
            >
              <ul class="picker-list" aria-label={gt("exercises")}>
                <For each={results()}>
                  {(exercise) => (
                    <li>
                      <button
                        type="button"
                        class="picker-item"
                        disabled={added().has(exercise.id) || adding() !== null}
                        onClick={() => void choose(exercise)}
                      >
                        <span class="picker-item-main">
                          <span class="picker-item-name">{exercise.name}</span>
                          <span class="muted small">
                            {exercise.muscle} · {exercise.equipment} · {kindLabel(exercise.kind)}
                          </span>
                        </span>
                        <Show when={added().has(exercise.id)}>
                          <span class="badge">{gt("added")}</span>
                        </Show>
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
            <button type="button" class="btn btn--block" onClick={() => setCreating(true)}>
              {gt("createExercise")}
            </button>
          </Show>
        </div>
      </div>
    </Show>
  );
}
