import { onCleanup, onMount, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import "../../styles/gallery-viewer.css";

const FOCUSABLE = 'a[href], button, input, select, textarea, [tabindex]:not([tabindex="-1"])';

/**
 * The Gallery's dialog: a labelled modal above the viewer (and the grid) that traps Tab, closes on Esc and on a click
 * outside, and gives focus back to what had it. Esc is not passed on, so it never also closes the viewer under it.
 */
export function Modal(props: {
  /** The id of the element that names the dialog. */
  labelledBy: string;
  describedBy?: string;
  onClose: () => void;
  /** A request is running: Esc and a click outside do nothing. */
  busy?: boolean;
  /** Where focus starts (the first control by default). */
  initialFocus?: () => HTMLElement | null | undefined;
  children: JSX.Element;
}) {
  let dialog: HTMLDivElement | undefined;

  const focusables = () =>
    [...(dialog?.querySelectorAll<HTMLElement>(FOCUSABLE) ?? [])].filter((el) => !el.hasAttribute("disabled") && el.getAttribute("aria-hidden") !== "true");

  onMount(() => {
    const previous = document.activeElement as HTMLElement | null;
    (props.initialFocus?.() ?? focusables()[0] ?? dialog)?.focus();
    onCleanup(() => {
      if (previous && previous.isConnected) previous.focus();
    });
  });

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      if (!props.busy) props.onClose();
      return;
    }
    if (e.key !== "Tab") return;
    const list = focusables();
    if (!list.length) {
      e.preventDefault();
      return;
    }
    const first = list[0]!;
    const last = list[list.length - 1]!;
    const active = document.activeElement;
    if (e.shiftKey && (active === first || active === dialog)) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && active === last) {
      e.preventDefault();
      first.focus();
    } else if (active && !dialog?.contains(active)) {
      e.preventDefault();
      first.focus();
    }
  };

  return (
    <Portal>
      <div
        class="gv-overlay"
        onMouseDown={(e) => {
          if (e.target === e.currentTarget && !props.busy) props.onClose();
        }}
      >
        <div
          ref={dialog}
          class="gv-dialog"
          role="dialog"
          aria-modal="true"
          aria-labelledby={props.labelledBy}
          aria-describedby={props.describedBy}
          tabindex="-1"
          onKeyDown={onKeyDown}
        >
          {props.children}
        </div>
      </div>
    </Portal>
  );
}
