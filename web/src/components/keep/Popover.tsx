import { createSignal, onCleanup, onMount, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { useKeepRoot } from "./ctx";

/**
 * A floating panel next to its anchor button: below it, or above when there is no room; closes on
 * Escape, on a press outside, and when the anchor scrolls away (the page resizes it, it does not).
 */
export function Popover(props: { anchor: HTMLElement | undefined; label: string; onClose: () => void; class?: string; children: JSX.Element }) {
  const root = useKeepRoot();
  let el: HTMLDivElement | undefined;
  const [pos, setPos] = createSignal<{ left: number; top: number } | null>(null);

  function place() {
    const a = props.anchor;
    if (!el || !a) return;
    const r = a.getBoundingClientRect();
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const left = Math.max(8, Math.min(r.left, vw - w - 8));
    let top = r.bottom + 4;
    if (top + h > vh - 8) top = r.top - h - 4 >= 8 ? r.top - h - 4 : Math.max(8, vh - h - 8);
    setPos({ left, top });
  }

  onMount(() => {
    place();
    el?.querySelector<HTMLElement>("input, button, [tabindex]")?.focus({ preventScroll: true });
    const outside = (e: Event) => {
      const target = e.target as Node | null;
      if (!target || el?.contains(target) || props.anchor?.contains(target)) return;
      props.onClose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        e.preventDefault();
        props.onClose();
        props.anchor?.focus();
      }
    };
    document.addEventListener("pointerdown", outside, true);
    document.addEventListener("keydown", key, true);
    window.addEventListener("resize", place);
    onCleanup(() => {
      document.removeEventListener("pointerdown", outside, true);
      document.removeEventListener("keydown", key, true);
      window.removeEventListener("resize", place);
    });
  });

  return (
    <Portal mount={root() ?? document.body}>
      <div
        ref={el}
        class={props.class ? `kp-pop ${props.class}` : "kp-pop"}
        role="dialog"
        aria-label={props.label}
        style={{ left: `${pos()?.left ?? 0}px`, top: `${pos()?.top ?? 0}px`, visibility: pos() ? "visible" : "hidden" }}
      >
        {props.children}
      </div>
    </Portal>
  );
}
