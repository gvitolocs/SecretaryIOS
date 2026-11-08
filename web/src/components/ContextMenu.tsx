import { createEffect, createSignal, For, onCleanup, Show, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { t } from "../i18n";
import "../styles/context-menu.css";

// A context menu for any list of things (the Gallery's photos first; Instagram, Pixiv and Films can reuse it): at the
// pointer for a right-click, a Ctrl-click or the context-menu key, kept inside the window; on a phone (a long press)
// or a narrow window the same actions as a bottom sheet. Arrow keys move, Enter runs, Esc closes; it also closes on
// a click outside, on scroll, when the window loses focus and when it resizes. Focus goes back where it was.

export interface MenuAction {
  id: string;
  label: string;
  icon?: JSX.Element;
  danger?: boolean;
  disabled?: boolean;
  /** A thin line above this action. */
  separator?: boolean;
  run: () => void | Promise<void>;
}

export interface MenuRequest {
  x: number;
  y: number;
  mode: "popover" | "sheet";
  /** The sheet's title line (what the actions apply to). */
  title?: string;
}

/** Below this width the menu is a sheet, whatever asked for it. */
const SHEET_BELOW = 600;
const MARGIN = 8;

/** A touch (long press) or a narrow window gets the sheet; a mouse or a key the menu at the pointer. */
export function menuMode(pointerType?: string): MenuRequest["mode"] {
  if (pointerType === "touch") return "sheet";
  return typeof window !== "undefined" && window.innerWidth < SHEET_BELOW ? "sheet" : "popover";
}

/** Where a menu of `w` × `h` opens for a pointer at (x, y): right and below it, else flipped, never off screen. */
export function placeMenu(x: number, y: number, w: number, h: number, vw: number, vh: number): { left: number; top: number } {
  const left = x + w + MARGIN <= vw ? x : Math.max(MARGIN, Math.min(x - w, vw - w - MARGIN));
  const top = y + h + MARGIN <= vh ? y : Math.max(MARGIN, Math.min(y - h, vh - h - MARGIN));
  return { left, top };
}

export function ContextMenu(props: { request: MenuRequest | null; actions: MenuAction[]; label: string; onClose: () => void }) {
  let panel: HTMLDivElement | undefined;
  const [active, setActive] = createSignal(0);
  const [place, setPlace] = createSignal<{ left: number; top: number } | null>(null);
  // The finger that opened the menu (a long press) lifts with a click on whatever row is under it: a row runs on a
  // click only once a press began inside the menu. Keyboard and assistive clicks (detail 0) always count.
  let armed = false;
  const enabled = () => props.actions.map((a, i) => (a.disabled ? -1 : i)).filter((i) => i >= 0);

  const items = () => [...(panel?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [])];
  const focusAt = (index: number) => {
    setActive(index);
    items()[index]?.focus({ preventScroll: true });
  };
  const step = (dir: 1 | -1) => {
    const list = enabled();
    if (!list.length) return;
    const at = list.indexOf(active());
    focusAt(list[(at + dir + list.length) % list.length]!);
  };

  async function run(action: MenuAction) {
    if (action.disabled) return;
    props.onClose();
    await action.run();
  }

  function onKeyDown(e: KeyboardEvent) {
    if (e.key === "ArrowDown") step(1);
    else if (e.key === "ArrowUp") step(-1);
    else if (e.key === "Home") focusAt(enabled()[0] ?? 0);
    else if (e.key === "End") focusAt(enabled().at(-1) ?? 0);
    else if (e.key === "Escape" || e.key === "Tab") props.onClose();
    else if (e.key === "Enter" || e.key === " ") {
      const action = props.actions[active()];
      if (action) void run(action);
    } else return;
    e.preventDefault();
    e.stopPropagation();
  }

  createEffect(() => {
    const request = props.request;
    if (!request) return;
    const opener = document.activeElement as HTMLElement | null;
    setPlace(null);
    armed = false;
    setActive(enabled()[0] ?? 0);
    // Measured once rendered: right and below the pointer, flipped near the edges.
    const frame = requestAnimationFrame(() => {
      if (request.mode === "popover" && panel) {
        const r = panel.getBoundingClientRect();
        setPlace(placeMenu(request.x, request.y, r.width, r.height, window.innerWidth, window.innerHeight));
      }
      focusAt(active());
    });
    const outside = (e: PointerEvent) => {
      if (panel && !panel.contains(e.target as Node)) props.onClose();
    };
    const close = () => props.onClose();
    // Scrolling the menu's own list (a long sheet) is not a reason to close.
    const scrolled = (e: Event) => {
      if (!panel?.contains(e.target as Node)) props.onClose();
    };
    document.addEventListener("pointerdown", outside, true);
    document.addEventListener("scroll", scrolled, true);
    window.addEventListener("blur", close);
    window.addEventListener("resize", close);
    onCleanup(() => {
      cancelAnimationFrame(frame);
      document.removeEventListener("pointerdown", outside, true);
      document.removeEventListener("scroll", scrolled, true);
      window.removeEventListener("blur", close);
      window.removeEventListener("resize", close);
      if (opener?.isConnected) opener.focus({ preventScroll: true });
    });
  });

  const row = (action: MenuAction, index: () => number) => (
    <>
      <Show when={action.separator && index() > 0}>
        <div class="cm-sep" role="separator" />
      </Show>
      <button
        type="button"
        role="menuitem"
        class="cm-item"
        classList={{ "cm-item--danger": !!action.danger }}
        tabindex={active() === index() ? 0 : -1}
        disabled={action.disabled}
        aria-disabled={action.disabled ? "true" : undefined}
        onPointerMove={() => !action.disabled && setActive(index())}
        onClick={(e) => {
          if (armed || e.detail === 0) void run(action);
        }}
      >
        <span class="cm-icon" aria-hidden="true">{action.icon}</span>
        <span class="cm-label">{action.label}</span>
      </button>
    </>
  );

  return (
    <Show when={props.request}>
      {(request) => (
        <Portal>
          <Show
            when={request().mode === "sheet"}
            fallback={
              <div
                ref={panel}
                class="cm-menu glass"
                role="menu"
                aria-label={props.label}
                style={{
                  left: `${place()?.left ?? request().x}px`,
                  top: `${place()?.top ?? request().y}px`,
                  visibility: place() ? "visible" : "hidden",
                }}
                onKeyDown={onKeyDown}
                onPointerDown={() => (armed = true)}
                onContextMenu={(e) => e.preventDefault()}
              >
                <For each={props.actions}>{row}</For>
              </div>
            }
          >
            {/* A tap on the backdrop closes it through the outside-pointerdown listener. Not its click: the click that
                ends the long press which opened the sheet lands here too. */}
            <div class="cm-backdrop">
              <div ref={panel} class="cm-sheet glass" role="menu" aria-label={props.label} onKeyDown={onKeyDown} onPointerDown={() => (armed = true)}>
                <Show when={request().title}>{(title) => <p class="cm-title">{title()}</p>}</Show>
                <For each={props.actions}>{row}</For>
                <button type="button" class="cm-item cm-cancel" onClick={(e) => (armed || e.detail === 0) && props.onClose()}>
                  <span class="cm-label">{t("common.cancel")}</span>
                </button>
              </div>
            </div>
          </Show>
        </Portal>
      )}
    </Show>
  );
}
