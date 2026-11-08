import { A } from "@solidjs/router";
import { createMemo, For, Show } from "solid-js";
import type { GymView, Workout } from "../../api/gym";
import { formatDate } from "../../lib/units";
import { JournalDesk } from "./DeskViews";
import { ArrowUpRightIcon, BookIcon } from "./icons";
import { EmptyCard, exerciseById, GymPage, JOURNAL_HREF, setsText } from "./shared";
import { gt } from "./strings";

// Journal (HistoryView.swift): finished workouts, newest first. On a desktop-wide
// page it is DeskViews.tsx `JournalDesk`, under the Gym header row.

function JournalItem(props: { view: GymView; workout: Workout }) {
  const label = () => props.view.workout_labels[props.workout.id];
  const names = createMemo(() =>
    props.workout.exercises
      .map((e) => exerciseById(props.view, e.exercise_id)?.name)
      .filter((n): n is string => Boolean(n))
      .join(" · "),
  );
  return (
    <li>
      <A href={`${JOURNAL_HREF}/${props.workout.id}`} class="card history-item">
        <span class="history-item-head">
          <span class="gym-eyebrow">{formatDate(props.workout.finished_ms ?? props.workout.started_ms)}</span>
          <span class="history-item-arrow">
            <ArrowUpRightIcon />
          </span>
        </span>
        <span class="history-title">{props.workout.title}</span>
        <Show when={label()}>
          {(l) => (
            <span class="muted small">
              {l().duration} · {setsText(l().sets)} · {l().volume}
            </span>
          )}
        </Show>
        <span class="muted small history-names">{names()}</span>
      </A>
    </li>
  );
}

export default function JournalView() {
  return (
    <GymPage>
      {(view, wide) => (
        <Show when={!wide()} fallback={<JournalDesk view={view()} />}>
          <h1 class="view-title gym-title">{gt("journal")}</h1>
          <Show
            when={view().state.history.length > 0}
            fallback={
              <EmptyCard
                icon={<BookIcon />}
                title={gt("storyTitle")}
                detail={gt("storyDetail")}
              />
            }
          >
            <ul class="gym-list" aria-label={gt("finishedWorkouts")}>
              <For each={view().state.history}>{(workout) => <JournalItem view={view()} workout={workout} />}</For>
            </ul>
          </Show>
        </Show>
      )}
    </GymPage>
  );
}
