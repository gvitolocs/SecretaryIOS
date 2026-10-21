import { A, useNavigate, useParams } from "@solidjs/router";
import { createMemo, createSignal, For, Show } from "solid-js";
import type { GymView, Workout, WorkoutExercise } from "../../api/gym";
import { formatDateTime } from "../../lib/units";
import { runGym } from "../../state/gym";
import { ConfirmButton, exerciseById, GymPage, SET_KIND_LABELS, setBadge, setsText, setValueText } from "./shared";

function ExerciseBlock(props: { view: GymView; item: WorkoutExercise }) {
  const exercise = createMemo(() => exerciseById(props.view, props.item.exercise_id));
  return (
    <section class="card" aria-label={exercise()?.name ?? "Exercise"}>
      <h2 class="exercise-name">{exercise()?.name ?? "Exercise"}</h2>
      <ol class="detail-sets">
        <For each={props.item.sets}>
          {(set, i) => (
            <li class="detail-set" classList={{ "detail-set--skipped": !set.completed }}>
              <span class="set-badge" classList={{ "set-badge--flag": set.kind !== "normal" }} title={SET_KIND_LABELS[set.kind]}>
                {setBadge(set.kind, i())}
              </span>
              <span class="sr-only">{SET_KIND_LABELS[set.kind]} set,</span>
              <span class="detail-set-value">{setValueText(props.view, set, exercise()?.kind ?? "weight_reps")}</span>
              <span class="detail-set-state">{set.completed ? "Done" : "Skipped"}</span>
            </li>
          )}
        </For>
      </ol>
      <Show when={props.item.notes}>
        <p class="muted small preserve">{props.item.notes}</p>
      </Show>
    </section>
  );
}

function Detail(props: { view: GymView; workout: Workout }) {
  const navigate = useNavigate();
  const [repeating, setRepeating] = createSignal(false);
  const label = () => props.view.workout_labels[props.workout.id];

  async function repeat() {
    if (repeating()) return;
    setRepeating(true);
    try {
      const res = await runGym({ type: "repeat_workout", workout_id: props.workout.id });
      if (res?.ok) navigate("/gym/workout");
    } finally {
      setRepeating(false);
    }
  }

  return (
    <>
      <A href="/gym/history" class="back-link">
        ‹ History
      </A>
      <h1 class="view-title">{props.workout.title}</h1>
      <p class="muted">
        {formatDateTime(props.workout.finished_ms ?? props.workout.started_ms)}
        <Show when={label()}>{(l) => ` · ${l().duration} · ${setsText(l().sets)} · ${l().volume}`}</Show>
      </p>
      <Show when={props.workout.notes}>
        <p class="preserve">{props.workout.notes}</p>
      </Show>
      <For each={props.workout.exercises}>{(item) => <ExerciseBlock view={props.view} item={item} />}</For>
      <div class="workout-actions">
        <button
          type="button"
          class="btn btn--primary"
          disabled={repeating() || Boolean(props.view.state.active)}
          onClick={() => void repeat()}
        >
          Repeat workout
        </button>
        <ConfirmButton
          label="Delete"
          confirmLabel="Delete workout"
          onConfirm={async () => {
            const res = await runGym({ type: "delete_workout", workout_id: props.workout.id });
            if (res?.ok) navigate("/gym/history");
          }}
        />
      </div>
      <Show when={props.view.state.active}>
        <p class="muted small">Finish or discard your current workout before repeating this one.</p>
      </Show>
    </>
  );
}

export default function HistoryDetailView() {
  const params = useParams();
  return (
    <GymPage>
      {(view) => (
        <Show
          when={view().state.history.find((w) => w.id === params.id)}
          fallback={
            <div class="empty">
              <p class="empty-title">Workout not found.</p>
              <A href="/gym/history" class="btn btn-link">
                Back to history
              </A>
            </div>
          }
        >
          {(workout) => <Detail view={view()} workout={workout()} />}
        </Show>
      )}
    </GymPage>
  );
}
