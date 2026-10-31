import { A, useParams } from "@solidjs/router";
import { createMemo, Show } from "solid-js";
import type { GymView } from "../../api/gym";
import { BarChart } from "./BarChart";
import { ExerciseDetailDesk } from "./DeskViews";
import { RecordMetrics } from "./Progress";
import { exerciseById, Eyebrow, GymPage } from "./shared";
import { gt } from "./strings";

function DetailBody(props: { view: GymView; exerciseId: string }) {
  const exercise = createMemo(() => exerciseById(props.view, props.exerciseId));
  const record = createMemo(() => props.view.records.find((r) => r.exercise_id === props.exerciseId));
  // Drawn only when the server sends at least two points (absent on older engines).
  const trend = createMemo(() => {
    const points = props.view.exercise_charts?.[props.exerciseId] ?? [];
    return points.length >= 2 ? points : null;
  });

  return (
    <>
      <A href="/gym/progress" class="back-link">
        {gt("backProgress")}
      </A>
      <h1 class="view-title gym-title">{exercise()?.name ?? record()?.name ?? gt("exercise")}</h1>
      <Show
        when={record()}
        fallback={
          <div class="empty">
            <p class="empty-title">{gt("noSetsYet")}</p>
            <p class="empty-sub">{gt("noSetsDetail")}</p>
          </div>
        }
      >
        {(r) => (
          <>
            <section class="card" aria-label={gt("bestSet")}>
              <Eyebrow>{gt("bestSet")}</Eyebrow>
              <RecordMetrics record={r()} kind={exercise()?.kind ?? "weight_reps"} />
              <Show when={(exercise()?.kind ?? "weight_reps") === "weight_reps"}>
                <p class="muted small">{gt("bestSetVolume", { volume: r().volume })}</p>
              </Show>
            </section>
            <Show when={r().estimated_1rm}>
              {(orm) => (
                <section class="card" aria-label={gt("estimated1rm")}>
                  <Eyebrow>{gt("estimated1rm")}</Eyebrow>
                  <p class="gym-big-number">{orm()}</p>
                </section>
              )}
            </Show>
          </>
        )}
      </Show>
      <section class="card" aria-label={gt("volumeTrend")}>
        <Eyebrow>{gt("volumeTrend")}</Eyebrow>
        <Show when={trend()} fallback={<p class="muted small">{gt("noTrend")}</p>}>
          {(t) => (
            <BarChart
              points={t()}
              unit={props.view.state.settings.weight_unit}
              ariaLabel={gt("trendLabel", { name: exercise()?.name ?? record()?.name ?? gt("exercise") })}
            />
          )}
        </Show>
      </section>
    </>
  );
}

export default function ProgressDetailView() {
  const params = useParams();
  return (
    <GymPage>
      {(view, wide) => (
        <Show when={wide()} fallback={<DetailBody view={view()} exerciseId={params.exerciseId ?? ""} />}>
          <ExerciseDetailDesk view={view()} exerciseId={params.exerciseId ?? ""} />
        </Show>
      )}
    </GymPage>
  );
}
