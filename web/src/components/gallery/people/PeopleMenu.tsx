import { createSignal, onCleanup, Show } from "solid-js";
import { GearIcon } from "../../Icons";
import "../../../styles/gallery-people.css";
import { groupText, useGroupKind } from "../../../lib/galleryGroup";

/** The settings menu on the People page: turn recognition off, delete what it found. */
export function PeopleMenu(props: { wide?: boolean; onTurnOff: () => void; onDelete: () => void }) {
  const kind = useGroupKind();
  const pt = groupText(kind);
  const [open, setOpen] = createSignal(false);
  let root: HTMLDivElement | undefined;
  const away = (e: PointerEvent) => {
    if (root && e.target instanceof Node && !root.contains(e.target)) setOpen(false);
  };
  document.addEventListener("pointerdown", away);
  onCleanup(() => document.removeEventListener("pointerdown", away));
  const choose = (action: () => void) => () => {
    setOpen(false);
    action();
  };
  return (
    <div class="pp-menu" ref={root} onKeyDown={(e) => e.key === "Escape" && open() && (e.stopPropagation(), setOpen(false))}>
      <button type="button" class={props.wide ? "pg-icon-btn" : "g-iconbtn"} aria-label={pt("settings")} title={pt("settings")} aria-haspopup="menu" aria-expanded={open()} onClick={() => setOpen((v) => !v)}>
        <GearIcon />
      </button>
      <Show when={open()}>
        <div class="pp-menu__list" role="menu">
          <button type="button" role="menuitem" class="pp-menu__item" onClick={choose(props.onTurnOff)}>
            {pt("turnOff")}
          </button>
          <button type="button" role="menuitem" class="pp-menu__item pp-menu__item--danger" onClick={choose(props.onDelete)}>
            {pt("deleteData")}
          </button>
        </div>
      </Show>
    </div>
  );
}
