import { createMemo, For, Show } from "solid-js";

export interface ChartSeries {
  key: string;
  points: { x: number; y: number }[];
  class?: string;
}

export interface LineChartProps {
  series: ChartSeries[];
  xLabels?: string[];
  ariaLabel: string;
  height?: number;
  formatY?: (value: number) => string;
}

function axisNumber(value: number): string {
  const rounded = Math.round(value * 10) / 10;
  return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
}

export function LineChart(props: LineChartProps) {
  const height = () => props.height ?? 160;

  const points = createMemo(() => props.series.flatMap((s) => s.points));
  const count = createMemo(() => Math.max(0, ...props.series.map((s) => s.points.length)));
  const bounds = createMemo(() => {
    const all = points();
    if (all.length === 0) return { minX: 0, maxX: 0, minY: 0, maxY: 0 };
    let minX = Infinity;
    let maxX = -Infinity;
    let minY = Infinity;
    let maxY = -Infinity;
    for (const p of all) {
      if (p.x < minX) minX = p.x;
      if (p.x > maxX) maxX = p.x;
      if (p.y < minY) minY = p.y;
      if (p.y > maxY) maxY = p.y;
    }
    return { minX, maxX, minY, maxY };
  });

  const formatY = (value: number) => (props.formatY ? props.formatY(value) : axisNumber(value));

  function linePoints(seriesPoints: { x: number; y: number }[]): string {
    const b = bounds();
    const h = height();
    const spanX = b.maxX - b.minX;
    const spanY = b.maxY - b.minY;
    return seriesPoints
      .map((p, index) => {
        const x =
          spanX === 0
            ? seriesPoints.length === 1
              ? 50
              : (index / (seriesPoints.length - 1)) * 100
            : ((p.x - b.minX) / spanX) * 100;
        const y = spanY === 0 ? h / 2 : h - ((p.y - b.minY) / spanY) * h;
        return `${x.toFixed(2)},${y.toFixed(2)}`;
      })
      .join(" ");
  }

  return (
    <div class="chart" role="img" aria-label={props.ariaLabel}>
      <Show
        when={count() >= 2}
        fallback={<p class="chart-empty muted small">Not enough data to chart yet.</p>}
      >
        <div class="chart-plot">
          <span class="chart-y chart-y--max">{formatY(bounds().maxY)}</span>
          <svg
            class="chart-svg"
            viewBox={`0 0 100 ${height()}`}
            preserveAspectRatio="none"
            height={height()}
            aria-hidden="true"
          >
            <For each={props.series}>
              {(series) => (
                <polyline
                  class={`chart-line ${series.class ?? ""}`}
                  points={linePoints(series.points)}
                  vector-effect="non-scaling-stroke"
                />
              )}
            </For>
          </svg>
          <span class="chart-y chart-y--min">{formatY(bounds().minY)}</span>
        </div>
        <Show when={props.xLabels && props.xLabels.length > 0}>
          <div class="chart-x">
            <For each={props.xLabels}>{(label) => <span>{label}</span>}</For>
          </div>
        </Show>
      </Show>
    </div>
  );
}
