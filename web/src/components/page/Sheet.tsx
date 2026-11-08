import { createEffect, onCleanup, Show, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { t } from "../../i18n";
import "../../styles/page.css";

// The sheet / dialog frame of the page system, from the exercise picker
// (views/gym/Picker.tsx): on a window at least 880 px wide a centred dialog,
// 640 px and at most 80 % of the height, whose frame owns the radius, border
// and clip; only the body scrolls. On a phone a bottom sheet. Escape and the
// backdrop close it, focus moves in on open and back to the opener on close.

const FOCUSABLE = 'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

let sheetSeq = 0;

export function Sheet(props: {
  open: boolean;
  onClose: () => void;
  title: JSX.Element;
  /** Fixed under the title: a search field, filter chips. */
  tools?: JSX.Element;
  /** Fixed at the bottom: the sheet's actions. */
  footer?: JSX.Element;
  /** "wide" is 800 px on a desktop, for side-by-side content. */
  size?: "default" | "wide";
  class?: string;
  children: JSX.Element;
}) {
  const id = `pg-sheet-${++sheetSeq}`;
  let dialog: HTMLDivElement | undefined;
  let anchor: HTMLSpanElement | undefined;
  // The page frame contains its layout (a size container), so the overlay is
  // portalled out of it, into the shell that sets the theme.
  const mount = () => anchor?.closest<HTMLElement>(".shell") ?? document.body;

  createEffect(() => {
    if (!props.open) return;
    const opener = document.activeElement as HTMLElement | null;
    queueMicrotask(() => {
      const inside = FOCUSABLE.split(", ").flatMap((s) => [`.pg-sheet-tools ${s}`, `.pg-sheet-body ${s}`]).join(", ");
      const first = dialog?.querySelector<HTMLElement>(inside);
      (first ?? dialog)?.focus();
    });
    onCleanup(() => opener?.focus?.());
  });

  function onKeyDown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      props.onClose();
      return;
    }
    if (e.key !== "Tab" || !dialog) return;
    // Keep Tab inside the dialog.
    const items = Array.from(dialog.querySelectorAll<HTMLElement>(FOCUSABLE));
    if (!items.length) return;
    const first = items[0]!;
    const last = items[items.length - 1]!;
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  }

  return (
    <>
      <span ref={anchor} hidden />
      <Show when={props.open}>
        <Portal mount={mount()}>
          <div class="pg-sheet-overlay" onClick={(e) => e.target === e.currentTarget && props.onClose()}>
            <div
              class={`pg-sheet${props.size === "wide" ? " pg-sheet--wide" : ""} ${props.class ?? ""}`}
              role="dialog"
              aria-modal="true"
              aria-labelledby={id}
              tabindex="-1"
              ref={dialog}
              onKeyDown={onKeyDown}
            >
              <header class="pg-sheet-head">
                <h2 class="pg-sheet-title" id={id}>
                  {props.title}
                </h2>
                <button type="button" class="pg-icon-btn" aria-label={t("common.close")} onClick={() => props.onClose()}>
                  <svg class="pg-icon" viewBox="0 0 24 24" aria-hidden="true">
                    <path d="M6 6l12 12M18 6L6 18" />
                  </svg>
                </button>
              </header>
              <Show when={props.tools}>
                <div class="pg-sheet-tools">{props.tools}</div>
              </Show>
              <div class="pg-sheet-body">{props.children}</div>
              <Show when={props.footer}>
                <div class="pg-sheet-foot">{props.footer}</div>
              </Show>
            </div>
          </div>
        </Portal>
      </Show>
    </>
  );
}
