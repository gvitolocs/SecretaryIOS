import { For, onMount, Show } from "solid-js";
import { moveSection, orderedSections, resetSectionOrder, shop, t } from "../../state/shopping";
import { SectionIcon } from "./ItemIcon";

/**
 * The store-walk order of the catalog sections (per list, kept on this device): the to-buy grid and
 * the catalog follow it. Up/down buttons move a section one place; focus stays on the moved row.
 */
export function SectionOrderSheet(props: { open: boolean; onClose: () => void }) {
  let sheet: HTMLDivElement | undefined;

  function move(id: string, delta: -1 | 1) {
    moveSection(id, delta);
    // Keep the pressed button under focus after the row moved.
    queueMicrotask(() => sheet?.querySelector<HTMLButtonElement>(`[data-move="${id}:${delta}"]`)?.focus());
  }

  return (
    <Show when={props.open}>
      <div class="bring-scrim" onClick={(e) => e.target === e.currentTarget && props.onClose()}>
        <div
          ref={sheet}
          class="bring-sheet shop-order"
          role="dialog"
          aria-modal="true"
          aria-labelledby="shop-order-title"
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.stopPropagation();
              props.onClose();
            }
          }}
        >
          <div class="bring-sheet-grip" aria-hidden="true" />
          <header class="bring-details-head">
            <h2 id="shop-order-title" class="bring-details-name">
              {t("sortSections")}
            </h2>
            <Show when={(shop.sectionOrder[shop.currentListId] ?? []).length > 0}>
              <button type="button" class="bring-text-btn" onClick={resetSectionOrder}>
                {t("resetOrder")}
              </button>
            </Show>
            <OrderDoneButton onClose={props.onClose} />
          </header>
          <p class="bring-muted shop-order-hint">{t("sortSectionsHint")}</p>
          <ol class="shop-order-list">
            <For each={orderedSections()}>
              {(sec, i) => (
                <li class="shop-order-row">
                  <span class="shop-order-n" aria-hidden="true">
                    {i() + 1}
                  </span>
                  <span class="shop-order-icon" aria-hidden="true">
                    <SectionIcon id={sec.id} icon={sec.icon} />
                  </span>
                  <span class="shop-order-name">{sec.name}</span>
                  <button
                    type="button"
                    class="shop-order-btn"
                    data-move={`${sec.id}:-1`}
                    aria-label={t("moveUp", { name: sec.name })}
                    disabled={i() === 0}
                    onClick={() => move(sec.id, -1)}
                  >
                    <span class="shop-arrow shop-arrow--up" aria-hidden="true" />
                  </button>
                  <button
                    type="button"
                    class="shop-order-btn"
                    data-move={`${sec.id}:1`}
                    aria-label={t("moveDown", { name: sec.name })}
                    disabled={i() === orderedSections().length - 1}
                    onClick={() => move(sec.id, 1)}
                  >
                    <span class="shop-arrow shop-arrow--down" aria-hidden="true" />
                  </button>
                </li>
              )}
            </For>
          </ol>
        </div>
      </div>
    </Show>
  );
}

function OrderDoneButton(props: { onClose: () => void }) {
  let btn: HTMLButtonElement | undefined;
  onMount(() => btn?.focus());
  return (
    <button ref={btn} type="button" class="bring-text-btn" onClick={() => props.onClose()}>
      {t("done")}
    </button>
  );
}
