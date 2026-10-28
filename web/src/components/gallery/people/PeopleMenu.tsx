import { createSignal, onCleanup, Show } from "solid-js";
import { t } from "../../../i18n";
import { GearIcon } from "../../Icons";
import "../../../styles/gallery-people.css";

/** The settings menu on the People page: turn recognition off, delete what it found. */
export function PeopleMenu(props: { onTurnOff: () => void; onDelete: () => void }) {
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
      <button type="button" class="g-iconbtn" aria-label={t("galleryPeople.settings")} title={t("galleryPeople.settings")} aria-haspopup="menu" aria-expanded={open()} onClick={() => setOpen((v) => !v)}>
        <GearIcon />
      </button>
      <Show when={open()}>
        <div class="pp-menu__list" role="menu">
          <button type="button" role="menuitem" class="pp-menu__item" onClick={choose(props.onTurnOff)}>
            {t("galleryPeople.turnOff")}
          </button>
          <button type="button" role="menuitem" class="pp-menu__item pp-menu__item--danger" onClick={choose(props.onDelete)}>
            {t("galleryPeople.deleteData")}
          </button>
        </div>
      </Show>
    </div>
  );
}
