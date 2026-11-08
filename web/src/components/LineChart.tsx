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
  /** At most this many small numbers on the line (first series), at its turning points. */
  marks?: number;
}

function axisNumber(value: number): string {
  const rounded = Math.round(value * 10) / 10;
  return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
}

/** A bend smaller than this share of the chart's height is noise, not a turning point. */
const MIN_BEND = 0.05;
/** Marks closer together than this share of the chart's width would run into each other. */
const MIN_GAP = 0.1;

export interface TurningPoint {
  index: number;
  /** The number goes above the line (else below it). */
  above: boolean;
}

/**
 * Where a line changes direction the most: it starts as a straight line from the first point to the
 * last and keeps adding the point furthest from it, so the biggest bends come first. The last point
 * (today's value) always counts; marks too close to a bigger one are left out.
 */
export function turningPoints(points: { x: number; y: number }[], max: number): TurningPoint[] {
  const n = points.length;
  if (n < 2 || max < 1) return [];
  const minX = points[0]!.x;
  const spanX = points[n - 1]!.x - minX;
  const ys = points.map((p) => p.y);
  const minY = Math.min(...ys);
  const spanY = Math.max(...ys) - minY;
  if (spanX <= 0 || spanY <= 0) return [];
  const nx = (i: number) => (points[i]!.x - minX) / spanX;
  const ny = (i: number) => (points[i]!.y - minY) / spanY;
  // How far point `i` lies above the straight line between `a` and `b`.
  const offLine = (i: number, a: number, b: number) => {
    const run = nx(b) - nx(a);
    const t = run > 0 ? (nx(i) - nx(a)) / run : 0;
    return ny(i) - (ny(a) + t * (ny(b) - ny(a)));
  };

  const kept = [0, n - 1];
  const marks = [n - 1];
  const free = (i: number) => marks.every((m) => Math.abs(nx(m) - nx(i)) >= MIN_GAP);
  while (marks.length < max) {
    let best = -1;
    let bestAt = 0;
    let bestOff = MIN_BEND;
    for (let k = 0; k + 1 < kept.length; k++) {
      for (let i = kept[k]! + 1; i < kept[k + 1]!; i++) {
        const off = Math.abs(offLine(i, kept[k]!, kept[k + 1]!));
        if (off > bestOff) {
          best = i;
          bestAt = k + 1;
          bestOff = off;
        }
      }
    }
    if (best < 0) break;
    kept.splice(bestAt, 0, best);
    if (free(best)) marks.push(best);
  }
  // The start, when the room is left.
  if (marks.length < max && free(0)) marks.push(0);

  return marks
    .sort((a, b) => a - b)
    .map((index) => {
      const at = kept.indexOf(index);
      const prev = kept[at - 1];
      const next = kept[at + 1];
      // The ends have one neighbour: a number sits on the open side of the line.
      const above = prev === undefined || next === undefined ? true : offLine(index, prev, next) >= 0;
      return { index, above };
    });
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

  // The small numbers along the line. The top and bottom already have theirs on the axis when they sit next to it.
  const marks = createMemo(() => {
    const line = props.series[0]?.points ?? [];
    const { minX, maxX, minY, maxY } = bounds();
    const spanX = maxX - minX;
    const spanY = maxY - minY;
    const h = height();
    return turningPoints(line, props.marks ?? 0).flatMap(({ index, above }) => {
      const p = line[index]!;
      const left = ((p.x - minX) / spanX) * 100;
      if (left < MIN_GAP * 100 && (p.y === maxY || p.y === minY)) return [];
      const top = h - ((p.y - minY) / spanY) * h;
      return [{ key: index, value: p.y, left, top, above }];
    });
  });

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
          <Show when={marks().length > 0}>
            <div class="chart-marks" aria-hidden="true">
              <For each={marks()}>
                {(m) => (
                  <span
                    class="chart-mark"
                    classList={{
                      "chart-mark--below": !m.above,
                      "chart-mark--start": m.left < 6,
                      "chart-mark--end": m.left > 94,
                    }}
                    style={{ left: `${m.left}%`, top: `${m.top}px` }}
                  >
                    {formatY(m.value)}
                  </span>
                )}
              </For>
            </div>
          </Show>
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
