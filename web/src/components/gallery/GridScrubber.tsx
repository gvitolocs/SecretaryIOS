import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { locale, t } from "../../i18n";
import { monthAtFraction, scrubMarks, type MonthCount } from "../../lib/galleryLayout";

export interface GridScrubberProps {
  months: MonthCount[];
  /** The month at the top of the grid (`YYYY-MM`). */
  current: string | null;
  /** Scroll to a month; the grid loads pages first when it is further down than what is loaded. */
  onSeek: (month: string) => void;
}

function monthName(month: string): string {
  const [y, m] = month.split("-").map(Number);
  try {
    return new Intl.DateTimeFormat(locale(), { month: "long", year: "numeric" }).format(new Date(y!, (m ?? 1) - 1, 1));
  } catch {
    return month;
  }
}

/** A thin month scrubber on the right edge: the years down the track, a thumb at the month in view, drag to jump. */
export function GridScrubber(props: GridScrubberProps) {
  let track!: HTMLDivElement;
  const [dragging, setDragging] = createSignal<string | null>(null);
  const [hover, setHover] = createSignal<string | null>(null);
  const [height, setHeight] = createSignal(600);
  onMount(() => {
    const measure = () => setHeight(track.clientHeight || 600);
    measure();
    let ro: ResizeObserver | undefined;
    if (typeof ResizeObserver !== "undefined") {
      ro = new ResizeObserver(measure);
      ro.observe(track);
    }
    onCleanup(() => ro?.disconnect());
  });
  const marks = createMemo(() => scrubMarks(props.months, height()));
  const index = () => Math.max(0, marks().findIndex((m) => m.month === props.current));
  const thumbAt = () => marks()[index()]?.at ?? 0;

  const monthAt = (clientY: number): string | null => {
    const box = track.getBoundingClientRect();
    return monthAtFraction(marks(), box.height ? (clientY - box.top) / box.height : 0);
  };
  let last: string | null = null;
  const go = (month: string | null) => {
    if (!month) return;
    setDragging(month);
    if (month !== last) {
      last = month;
      props.onSeek(month);
    }
  };
  const step = (delta: number) => {
    const next = marks()[Math.min(marks().length - 1, Math.max(0, index() + delta))];
    if (next) props.onSeek(next.month);
  };

  return (
    <div
      class="g-scrubber"
      role="slider"
      tabindex="0"
      aria-orientation="vertical"
      aria-label={t("gallery.scrubber")}
      aria-valuemin={0}
      aria-valuemax={Math.max(0, marks().length - 1)}
      aria-valuenow={index()}
      aria-valuetext={props.current ? monthName(props.current) : ""}
      classList={{ "g-scrubber--drag": dragging() !== null }}
      onKeyDown={(e) => {
        if (e.key === "ArrowDown" || e.key === "PageDown") (e.preventDefault(), step(1));
        else if (e.key === "ArrowUp" || e.key === "PageUp") (e.preventDefault(), step(-1));
        else if (e.key === "Home") (e.preventDefault(), step(-marks().length));
        else if (e.key === "End") (e.preventDefault(), step(marks().length));
      }}
      onPointerDown={(e) => {
        e.preventDefault();
        (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
        last = null;
        go(monthAt(e.clientY));
      }}
      onPointerMove={(e) => {
        if (dragging() !== null) go(monthAt(e.clientY));
        else setHover(monthAt(e.clientY));
      }}
      onPointerUp={() => {
        setDragging(null);
      }}
      onPointerCancel={() => setDragging(null)}
      onPointerLeave={() => setHover(null)}
    >
      <div class="g-scrubber__track" ref={track}>
        <For each={marks()}>
          {(m) => (
            <>
              <Show when={m.year}>
                <span class="g-scrubber__year" style={{ top: `${m.at * 100}%` }}>
                  {m.year}
                </span>
              </Show>
              <span class="g-scrubber__tick" style={{ top: `${m.at * 100}%` }} />
            </>
          )}
        </For>
        <span class="g-scrubber__thumb" style={{ top: `${thumbAt() * 100}%` }} />
        <Show when={dragging() ?? hover()}>
          {(m) => (
            <span class="g-scrubber__bubble" style={{ top: `${(marks().find((x) => x.month === m())?.at ?? thumbAt()) * 100}%` }}>
              {monthName(m())}
            </span>
          )}
        </Show>
      </div>
    </div>
  );
}
