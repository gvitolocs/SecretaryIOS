import { A, useParams } from "@solidjs/router";
import { createMemo, Show } from "solid-js";
import type { GymView } from "../../api/gym";
import { LineChart } from "../../components/LineChart";
import { RecordMetrics, volumeChart } from "./Progress";
import { exerciseById, GymPage } from "./shared";

function DetailBody(props: { view: GymView; exerciseId: string }) {
  const exercise = createMemo(() => exerciseById(props.view, props.exerciseId));
  const record = createMemo(() => props.view.records.find((r) => r.exercise_id === props.exerciseId));
  // Drawn only when the server sends at least two points (absent on older engines).
  const trend = createMemo(() => {
    const points = props.view.exercise_charts?.[props.exerciseId] ?? [];
    return points.length >= 2 ? volumeChart(points) : null;
  });

  return (
    <>
      <A href="/gym/progress" class="back-link">
        ‹ Progress
      </A>
      <h1 class="view-title">{exercise()?.name ?? record()?.name ?? "Exercise"}</h1>
      <Show
        when={record()}
        fallback={
          <div class="empty">
            <p class="empty-title">No completed sets yet.</p>
            <p class="empty-sub">Finish a working set of this exercise to see its records.</p>
          </div>
        }
      >
        {(r) => (
          <>
            <section class="card" aria-label="Best set">
              <p class="eyebrow">Best set</p>
              <RecordMetrics record={r()} kind={exercise()?.kind ?? "weight_reps"} />
              <Show when={(exercise()?.kind ?? "weight_reps") === "weight_reps"}>
                <p class="muted small">Best set volume · {r().volume}</p>
              </Show>
            </section>
            <Show when={r().estimated_1rm}>
              {(orm) => (
                <section class="card" aria-label="Estimated 1RM">
                  <p class="eyebrow">Estimated 1RM</p>
                  <p class="big-number">{orm()}</p>
                </section>
              )}
            </Show>
          </>
        )}
      </Show>
      <section class="card" aria-label="Volume trend">
        <p class="eyebrow">Volume trend</p>
        <Show when={trend()} fallback={<p class="muted small">No per-exercise trend yet.</p>}>
          {(t) => (
            <LineChart
              series={t().series}
              xLabels={t().labels}
              ariaLabel={`Volume trend for ${exercise()?.name ?? "this exercise"}`}
              height={140}
            />
          )}
        </Show>
      </section>
    </>
  );
}

export default function ProgressDetailView() {
  const params = useParams();
  return <GymPage>{(view) => <DetailBody view={view()} exerciseId={params.exerciseId ?? ""} />}</GymPage>;
}
