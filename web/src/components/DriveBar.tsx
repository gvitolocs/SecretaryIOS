// The one toolbar over a remote desktop in focus mode, RustDesk-style: who drives, take over / give
// back, the viewer's own actions, a key popover and exit. It sits on the top edge (or the bottom one),
// can be dragged along it, and folds into a thin tab that never covers more than a few pixels of her
// screen; hovering or clicking the tab opens it. Where it sits and whether it is folded are remembered
// on this device. Shared by the Cua viewer page and LucinaDesk's player.

import { createSignal, For, onCleanup, Show, type JSX } from "solid-js";
import { t } from "../i18n";
import { readJSON, writeJSON } from "../lib/storage";
import "../styles/drivebar.css";

/** A button mirrored into the bar (the viewer's own toolbar, the keyboard…). */
export interface DriveBarAction {
  id: string;
  label: string;
  /** An icon shown instead of the label (the label stays the accessible name). */
  icon?: JSX.Element;
  pressed?: boolean;
  disabled?: boolean;
  run: () => void;
}

export interface DriveBarProps {
  /** Whether this page drives the desktop (else it watches). */
  driving: boolean;
  /** "You're driving", "Watching", "Reconnecting…". */
  status: string;
  busy?: boolean;
  onTakeOver: () => void;
  onGiveBack: () => void;
  onExit: () => void;
  /** Shown while driving, before the popover toggle. */
  actions?: DriveBarAction[];
  /** The popover's content (keys, clipboard): a "Keys" toggle appears when given. */
  panel?: JSX.Element;
  panelOpen?: boolean;
  onPanel?: (open: boolean) => void;
  /** After any button: give the keyboard back to the remote desktop. */
  onInteract?: () => void;
  /** localStorage key for where it sits and whether it is folded (per device). */
  storageKey?: string;
}

/** Where the bar sits: an edge and a horizontal fraction (0 left … 1 right). */
export interface DriveBarPlace {
  edge: "top" | "bottom";
  x: number;
  folded: boolean;
}

/** Line icons for the bar (no emoji: a desktop without an emoji font shows boxes). */
const svg = (d: string) => (
  <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <path d={d} />
  </svg>
);
export const barIcons = {
  scale: () => svg("M4 10V4h6M20 14v6h-6M4 4l7 7M20 20l-7-7"),
  clipboard: () => svg("M9 4h6v3H9zM8 5H6v15h12V5h-2M9 12h6M9 16h4"),
  audio: () => svg("M4 9v6h4l5 4V5L8 9zM16 9a4 4 0 0 1 0 6M18.5 6.5a8 8 0 0 1 0 11"),
  stats: () => svg("M5 20V11M12 20V4M19 20v-7"),
  keyboard: () => svg("M3 6h18v12H3zM7 10h.01M11 10h.01M15 10h.01M7 14h10"),
  chat: () => svg("M4 5h16v11H9l-5 4z"),
  up: () => svg("M6 15l6-6 6 6"),
  down: () => svg("M6 9l6 6 6-6"),
  pin: () => svg("M12 17v5M8 3h8l-1 7 3 3H6l3-3z"),
  close: () => svg("M6 6l12 12M18 6L6 18"),
};

export const DEFAULT_PLACE: DriveBarPlace = { edge: "top", x: 0.5, folded: false };
/** How long a peek (hovering the folded tab) outlives the pointer leaving the bar. */
export const PEEK_CLOSE_MS = 700;

export function readPlace(key: string): DriveBarPlace {
  const v = readJSON("local", key) as Partial<DriveBarPlace> | null;
  if (!v || typeof v !== "object") return { ...DEFAULT_PLACE };
  return {
    edge: v.edge === "bottom" ? "bottom" : "top",
    x: typeof v.x === "number" && Number.isFinite(v.x) ? clamp01(v.x) : 0.5,
    folded: v.folded === true,
  };
}

/** Where a drag ending at (clientX, clientY) in a viewport of that size puts the bar. */
export function placeAt(clientX: number, clientY: number, width: number, height: number, folded: boolean): DriveBarPlace {
  return { edge: clientY > height / 2 ? "bottom" : "top", x: clamp01(width > 0 ? clientX / width : 0.5), folded };
}

function clamp01(v: number): number {
  return Math.min(Math.max(v, 0.05), 0.95);
}

export default function DriveBar(props: DriveBarProps) {
  const key = () => props.storageKey ?? "drivebar.place";
  const [place, setPlace] = createSignal(readPlace(key()));
  const [peek, setPeek] = createSignal(false);
  const [drag, setDrag] = createSignal<{ x: number; y: number } | null>(null);
  let peekTimer: number | undefined;
  onCleanup(() => window.clearTimeout(peekTimer));

  const save = (p: DriveBarPlace) => {
    setPlace(p);
    writeJSON("local", key(), p);
  };
  const open = () => !place().folded || peek() || !!props.panelOpen;
  /** A button: act, then hand the keyboard back to her desktop. */
  const act = (fn: () => void) => () => {
    fn();
    props.onInteract?.();
  };
  // Buttons never take focus from the stream (the remote desktop keeps the keyboard).
  const keepFocus = (e: MouseEvent) => e.preventDefault();

  const startDrag = (e: PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    setDrag({ x: e.clientX, y: e.clientY });
  };
  const moveDrag = (e: PointerEvent) => {
    if (drag()) setDrag({ x: e.clientX, y: e.clientY });
  };
  const endDrag = (e: PointerEvent) => {
    const d = drag();
    if (!d) return;
    setDrag(null);
    save(placeAt(e.clientX, e.clientY, window.innerWidth, window.innerHeight, place().folded));
    props.onInteract?.();
  };

  const style = () => {
    const d = drag();
    const p = place();
    const left = d ? `${d.x}px` : `${p.x * 100}%`;
    return { left, ...(d ? { top: `${d.y}px` } : {}) } as JSX.CSSProperties;
  };

  return (
    <div
      class="drivebar"
      role="toolbar"
      aria-label={t("computer.fullScreenBar")}
      data-edge={place().edge}
      data-open={open() ? "yes" : "no"}
      data-dragging={drag() ? "yes" : "no"}
      data-mine={props.driving ? "yes" : "no"}
      style={style()}
      onMouseEnter={() => {
        window.clearTimeout(peekTimer);
        if (place().folded) setPeek(true);
      }}
      onMouseLeave={() => {
        window.clearTimeout(peekTimer);
        peekTimer = window.setTimeout(() => setPeek(false), PEEK_CLOSE_MS);
      }}
    >
      <Show
        when={open()}
        fallback={
          <button
            type="button"
            class="drivebar-tab"
            aria-label={`${props.status} — ${t("computer.showBar")}`}
            title={props.status}
            onMouseDown={keepFocus}
            onClick={() => setPeek(true)}
          >
            <span class="computer-dot" aria-hidden="true" />
          </button>
        }
      >
        <span
          class="drivebar-grip"
          aria-hidden="true"
          title={t("computer.moveBar")}
          onPointerDown={startDrag}
          onPointerMove={moveDrag}
          onPointerUp={endDrag}
          onPointerCancel={() => setDrag(null)}
        >
          ⋮⋮
        </span>
        <span class="drivebar-state" role="status">
          <span class="computer-dot" aria-hidden="true" />
          {props.status}
        </span>
        <Show
          when={props.driving}
          fallback={
            <button type="button" class="drivebar-btn drivebar-btn--primary" disabled={props.busy} onMouseDown={keepFocus} onClick={act(props.onTakeOver)}>
              {t("computer.takeOver")}
            </button>
          }
        >
          <button type="button" class="drivebar-btn" disabled={props.busy} onMouseDown={keepFocus} onClick={act(props.onGiveBack)}>
            {t("computer.giveBack")}
          </button>
          <For each={props.actions ?? []}>
            {(a) => (
              <button
                type="button"
                class="drivebar-btn"
                classList={{ "drivebar-btn--icon": !!a.icon }}
                aria-label={a.label}
                title={a.label}
                aria-pressed={a.pressed === undefined ? undefined : a.pressed}
                disabled={a.disabled}
                onMouseDown={keepFocus}
                onClick={act(a.run)}
              >
                {a.icon ?? a.label}
              </button>
            )}
          </For>
          <Show when={props.panel}>
            <button
              type="button"
              class="drivebar-btn"
              aria-pressed={!!props.panelOpen}
              aria-expanded={!!props.panelOpen}
              onMouseDown={keepFocus}
              onClick={() => props.onPanel?.(!props.panelOpen)}
            >
              {t("computer.keys")}
            </button>
          </Show>
        </Show>
        <button
          type="button"
          class="drivebar-btn drivebar-btn--icon"
          aria-label={place().folded ? t("computer.pinBar") : t("computer.foldBar")}
          title={place().folded ? t("computer.pinBar") : t("computer.foldBar")}
          aria-pressed={!place().folded}
          onMouseDown={keepFocus}
          onClick={act(() => {
            save({ ...place(), folded: !place().folded });
            setPeek(false);
          })}
        >
          {place().folded ? barIcons.pin() : place().edge === "top" ? barIcons.up() : barIcons.down()}
        </button>
        <button type="button" class="drivebar-btn drivebar-btn--icon" aria-label={t("computer.exitFull")} title={t("computer.exitFull")} onMouseDown={keepFocus} onClick={() => props.onExit()}>
          {barIcons.close()}
        </button>
        <Show when={props.panelOpen && props.panel}>
          <div class="drivebar-pop" role="group" aria-label={t("computer.keys")}>
            {props.panel}
          </div>
        </Show>
      </Show>
    </div>
  );
}
