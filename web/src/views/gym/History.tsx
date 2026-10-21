import { A } from "@solidjs/router";
import { createMemo, For, Show } from "solid-js";
import type { GymView, Workout } from "../../api/gym";
import { formatDate } from "../../lib/units";
import { exerciseById, GymPage, setsText } from "./shared";

function HistoryItem(props: { view: GymView; workout: Workout }) {
  const label = () => props.view.workout_labels[props.workout.id];
  const names = createMemo(() =>
    props.workout.exercises
      .map((e) => exerciseById(props.view, e.exercise_id)?.name)
      .filter((n): n is string => Boolean(n))
      .join(" · "),
  );
  return (
    <li>
      <A href={`/gym/history/${props.workout.id}`} class="card history-item">
        <span class="eyebrow">{formatDate(props.workout.finished_ms ?? props.workout.started_ms)}</span>
        <span class="history-title">{props.workout.title}</span>
        <Show when={label()}>
          {(l) => (
            <span class="small">
              {l().duration} · {setsText(l().sets)} · {l().volume}
            </span>
          )}
        </Show>
        <span class="muted small history-names">{names()}</span>
      </A>
    </li>
  );
}

export default function HistoryView() {
  return (
    <GymPage>
      {(view) => (
        <>
          <h1 class="view-title">History</h1>
          <Show
            when={view().state.history.length > 0}
            fallback={
              <div class="empty">
                <p class="empty-title">No workouts yet.</p>
                <p class="empty-sub">Finish a workout and it will be saved here.</p>
              </div>
            }
          >
            <ul class="gym-list" aria-label="Finished workouts">
              <For each={view().state.history}>{(workout) => <HistoryItem view={view()} workout={workout} />}</For>
            </ul>
          </Show>
        </>
      )}
    </GymPage>
  );
}
