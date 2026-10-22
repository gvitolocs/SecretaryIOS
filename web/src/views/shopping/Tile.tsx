import { onCleanup, Show } from "solid-js";
import { ItemIcon } from "./ItemIcon";

/**
 * Tile colours follow the Bring! roles: `need` red (to buy), `recent` and
 * `catalog` teal, `custom` grey-blue (a suggestion for free text). A catalog
 * tile that is already on the list shows red (`selected`).
 */
export type TileKind = "need" | "recent" | "catalog" | "custom";

const LONG_PRESS_MS = 480;
const MOVE_TOLERANCE_PX = 10;

export interface TileProps {
  flipId: string;
  kind: TileKind;
  icon: string | null;
  /** Catalog entry, for its bundled illustration. */
  catalogId?: string | null | undefined;
  name: string;
  spec?: string;
  /** Accessible label of the main button. */
  label: string;
  /** Catalog result that is already on the list (shown as a to-buy tile). */
  selected?: boolean;
  /** "Recently" edit mode: the tile toggles selection and shows a check mark. */
  checked?: boolean | undefined;
  onTap: () => void;
  /** Opens the details sheet (long-press, right-click, Shift+F10, or the focus-only ⓘ button). */
  onDetails?: () => void;
  detailsLabel?: string;
}

/** First letter for items without an emoji. */
function initial(name: string): string {
  return Array.from(name.trim())[0]?.toUpperCase() ?? "?";
}

export function Tile(props: TileProps) {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let start: { x: number; y: number } | null = null;
  /** When a long-press (or context menu) last opened details; the click right after it is swallowed. */
  let longPressAt = 0;
  const SWALLOW_MS = 700;

  const cancel = () => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    start = null;
  };
  onCleanup(cancel);

  function onPointerDown(e: PointerEvent) {
    if (!props.onDetails || props.checked !== undefined || (e.pointerType === "mouse" && e.button !== 0)) return;
    start = { x: e.clientX, y: e.clientY };
    timer = setTimeout(() => {
      timer = null;
      longPressAt = Date.now();
      props.onDetails?.();
    }, LONG_PRESS_MS);
  }

  function onPointerMove(e: PointerEvent) {
    if (!start) return;
    if (Math.hypot(e.clientX - start.x, e.clientY - start.y) > MOVE_TOLERANCE_PX) cancel();
  }

  function onClick() {
    // The click that ends a long-press must not also toggle the tile.
    if (Date.now() - longPressAt < SWALLOW_MS) {
      longPressAt = 0;
      return;
    }
    props.onTap();
  }

  /** Bring autosizes names between 14 and 12 sp; long names get the smaller size. */
  const long = () => props.name.length > 13;

  return (
    <div
      class="tile"
      classList={{
        "tile--need": props.kind === "need" || !!props.selected,
        "tile--recent": props.kind === "recent",
        "tile--catalog": props.kind === "catalog" && !props.selected,
        "tile--custom": props.kind === "custom",
        "tile--checked": props.checked === true,
        "tile--editing": props.checked !== undefined,
        "tile--has-spec": !!props.spec,
      }}
      data-flip-id={props.flipId}
      role="listitem"
    >
      <button
        type="button"
        class="tile-main"
        data-tile=""
        aria-label={props.label}
        aria-pressed={props.checked !== undefined ? props.checked : props.kind === "catalog" ? !!props.selected : undefined}
        onClick={onClick}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={cancel}
        onPointerLeave={cancel}
        onPointerCancel={cancel}
        onKeyDown={(e) => {
          if (props.onDetails && (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey))) {
            e.preventDefault();
            props.onDetails();
          }
        }}
        onContextMenu={(e) => {
          // Long-press on touch opens the browser menu otherwise; right-click opens details.
          if (!props.onDetails || props.checked !== undefined) return;
          e.preventDefault();
          if (Date.now() - longPressAt < SWALLOW_MS) return;
          cancel();
          // Swallow the click a touch long-press may still send right after the menu event.
          longPressAt = Date.now();
          props.onDetails();
        }}
      >
        <span class="tile-icon" aria-hidden="true">
          <Show when={props.icon} fallback={<span class="tile-initial">{props.kind === "custom" ? "+" : initial(props.name)}</span>}>
            {(icon) => <ItemIcon icon={icon()} catalogId={props.catalogId} />}
          </Show>
        </span>
        <span class="tile-name" classList={{ "tile-name--long": long() }}>
          {props.name}
        </span>
        <span class="tile-spec">{props.spec ?? ""}</span>
        <Show when={props.checked !== undefined}>
          <span class="tile-check" aria-hidden="true" />
        </Show>
      </button>
      <Show when={props.onDetails && props.checked === undefined}>
        <button type="button" class="tile-info" aria-label={props.detailsLabel ?? "Details"} onClick={() => props.onDetails?.()}>
          <span aria-hidden="true">i</span>
        </button>
      </Show>
    </div>
  );
}

/**
 * Arrow-key navigation between tiles: Left/Right step through the reading
 * order, Up/Down pick the nearest tile in the row above/below, Home/End jump.
 */
export function onTileKeyDown(e: KeyboardEvent, root: HTMLElement | undefined): void {
  if (!root || !["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End"].includes(e.key)) return;
  const target = e.target as HTMLElement | null;
  if (!target?.matches("[data-tile]")) return;
  const all = Array.from(root.querySelectorAll<HTMLElement>("[data-tile]"));
  const i = all.indexOf(target);
  if (i < 0) return;
  let next: HTMLElement | undefined;
  if (e.key === "ArrowLeft") next = all[i - 1];
  else if (e.key === "ArrowRight") next = all[i + 1];
  else if (e.key === "Home") next = all[0];
  else if (e.key === "End") next = all[all.length - 1];
  else {
    const here = target.getBoundingClientRect();
    const cx = here.left + here.width / 2;
    const down = e.key === "ArrowDown";
    const candidates = all
      .map((el) => ({ el, r: el.getBoundingClientRect() }))
      .filter(({ r }) => (down ? r.top > here.top + 1 : r.top < here.top - 1));
    if (candidates.length) {
      const rowTop = down ? Math.min(...candidates.map((c) => c.r.top)) : Math.max(...candidates.map((c) => c.r.top));
      const row = candidates.filter((c) => Math.abs(c.r.top - rowTop) < 2);
      row.sort((a, b) => Math.abs(a.r.left + a.r.width / 2 - cx) - Math.abs(b.r.left + b.r.width / 2 - cx));
      next = row[0]?.el;
    } else if (here.width === 0) {
      // No layout (tests): fall back to a 3-column reading order.
      next = all[down ? i + 3 : i - 3];
    }
  }
  if (next) {
    e.preventDefault();
    next.focus();
  }
}
