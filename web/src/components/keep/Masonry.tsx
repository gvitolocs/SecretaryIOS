import { createMemo, createSignal, For, onCleanup, onMount, Show, untrack, type JSX } from "solid-js";
import { gridMetrics, masonry } from "../../lib/keepLayout";

/**
 * Keep's grid: cards of 240 px with 16 px gaps, centred, placed in order into the shortest column
 * (so the order reads left to right). Heights come from ResizeObserver; without it (tests) the
 * cards simply flow in one column.
 */
export function Masonry<T extends { id: string }>(props: {
  items: T[];
  list: boolean;
  heading?: string;
  /** Called with the dragged and target ids when a card is dropped on another. */
  onDrop?: (dragged: string, target: string) => void;
  canDrag?: boolean;
  children: (item: T, drag: { draggable: boolean; onDragStart: (e: DragEvent) => void; onDragEnd: () => void }) => JSX.Element;
}) {
  const supported = typeof ResizeObserver !== "undefined";
  const [width, setWidth] = createSignal(0);
  const [heights, setHeights] = createSignal<Record<string, number>>({});
  const [ready, setReady] = createSignal(false);
  const [dragId, setDragId] = createSignal<string | null>(null);
  const [overId, setOverId] = createSignal<string | null>(null);
  let host: HTMLDivElement | undefined;
  const cells = new Map<string, HTMLElement>();
  let cellObserver: ResizeObserver | undefined;
  let frame = 0;

  const metrics = createMemo(() => gridMetrics(width(), props.list));
  const layout = createMemo(() =>
    masonry(
      props.items.map((i) => heights()[i.id] ?? 120),
      metrics().columns,
      metrics().gap,
    ),
  );

  function measure() {
    frame = 0;
    const next: Record<string, number> = {};
    let changed = false;
    const prev = heights();
    for (const [id, el] of cells) {
      const h = el.offsetHeight;
      next[id] = h;
      if (prev[id] !== h) changed = true;
    }
    if (changed || Object.keys(prev).length !== cells.size) setHeights(next);
    if (!ready() && cells.size > 0 && [...cells.values()].every((el) => el.offsetHeight > 0)) setReady(true);
    if (!ready() && cells.size === 0) setReady(true);
  }
  const schedule = () => {
    if (!frame) frame = requestAnimationFrame(measure);
  };

  onMount(() => {
    if (!supported || !host) return;
    const parent = host.parentElement ?? host;
    const widthObserver = new ResizeObserver(() => setWidth(parent.clientWidth));
    widthObserver.observe(parent);
    setWidth(parent.clientWidth);
    cellObserver = new ResizeObserver(schedule);
    for (const el of cells.values()) cellObserver.observe(el);
    schedule();
    onCleanup(() => {
      widthObserver.disconnect();
      cellObserver?.disconnect();
      if (frame) cancelAnimationFrame(frame);
    });
  });

  const register = (id: string, el: HTMLElement) => {
    cells.set(id, el);
    cellObserver?.observe(el);
    schedule();
    onCleanup(() => {
      cells.delete(id);
      cellObserver?.unobserve(el);
      schedule();
    });
  };

  return (
    <section class="kp-section" aria-label={props.heading}>
      <Show when={props.heading}>
        <h2 class="kp-section-head" style={supported ? { width: `${metrics().width}px` } : undefined}>
          {props.heading}
        </h2>
      </Show>
      <div
        ref={host}
        class="kp-masonry"
        classList={{ "kp-masonry--flow": !supported, "kp-masonry--ready": ready() }}
        style={supported ? { width: `${metrics().width}px`, height: `${layout().height}px` } : undefined}
        data-columns={supported ? metrics().columns : 1}
      >
        <For each={props.items}>
          {(item, index) => {
            const pos = () => layout().items[index()] ?? { col: 0, y: 0 };
            const m = metrics;
            return (
              <div
                class="kp-cell"
                classList={{ "kp-cell--drag": dragId() === item.id, "kp-cell--over": overId() === item.id && dragId() !== item.id }}
                data-id={item.id}
                ref={(el) => register(item.id, el)}
                style={
                  supported
                    ? { width: `${m().colWidth}px`, transform: `translate(${pos().col * (m().colWidth + m().gap)}px, ${pos().y}px)` }
                    : undefined
                }
                onDragOver={(e) => {
                  if (!dragId() || dragId() === item.id) return;
                  e.preventDefault();
                  setOverId(item.id);
                }}
                onDrop={(e) => {
                  const from = dragId();
                  if (!from || from === item.id) return;
                  e.preventDefault();
                  setDragId(null);
                  setOverId(null);
                  props.onDrop?.(from, item.id);
                }}
              >
                {/* Rendered once per card: a signal read here (canDrag, the view behind it) must not rebuild every
                    card, which made opening and closing a note take half a second on 400 notes (2026-10-31). */}
                {untrack(() =>
                  props.children(item, {
                  get draggable() {
                    return !!props.canDrag;
                  },
                  onDragStart: (e) => {
                    setDragId(item.id);
                    if (e.dataTransfer) {
                      e.dataTransfer.effectAllowed = "move";
                      e.dataTransfer.setData("text/plain", item.id);
                    }
                  },
                  onDragEnd: () => {
                    setDragId(null);
                    setOverId(null);
                  },
                  }),
                )}
              </div>
            );
          }}
        </For>
      </div>
    </section>
  );
}
