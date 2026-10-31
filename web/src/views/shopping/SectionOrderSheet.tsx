import { type JSX, For, onMount, Show } from "solid-js";
import { moveSection, orderedSections, resetSectionOrder, shop, t } from "../../state/shopping";
import { Button } from "../../components/page";
import { SectionIcon } from "./ItemIcon";
import { ShopSheet } from "./ShopSheet";

/**
 * The store-walk order of the catalog sections (per list, kept on this device): the to-buy grid and
 * the catalog follow it. Up/down buttons move a section one place; focus stays on the moved row.
 */
export function SectionOrderSheet(props: { wide?: boolean; open: boolean; onClose: () => void }) {
  let sheet: HTMLDivElement | undefined;

  function move(id: string, delta: -1 | 1) {
    moveSection(id, delta);
    // Keep the pressed button under focus after the row moved.
    queueMicrotask(() => sheet?.querySelector<HTMLButtonElement>(`[data-move="${id}:${delta}"]`)?.focus());
  }

  const hasCustomOrder = () => (shop.sectionOrder[shop.currentListId] ?? []).length > 0;

  const Frame = (p: { children: JSX.Element }) => (
    <Show
      when={props.wide}
      fallback={
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
            {p.children}
          </div>
        </div>
      }
    >
      <ShopSheet
        open
        title={t("sortSections")}
        onClose={props.onClose}
        footer={
          <>
            <Show when={hasCustomOrder()}>
              <Button onClick={resetSectionOrder}>{t("resetOrder")}</Button>
            </Show>
            <Button onClick={props.onClose}>{t("done")}</Button>
          </>
        }
      >
        <div ref={sheet}>{p.children}</div>
      </ShopSheet>
    </Show>
  );

  return (
    <Show when={props.open}>
      <Frame>
        <Show when={!props.wide}>
          <div class="bring-sheet-grip" aria-hidden="true" />
          <header class="bring-details-head">
            <h2 id="shop-order-title" class="bring-details-name">
              {t("sortSections")}
            </h2>
            <Show when={hasCustomOrder()}>
              <button type="button" class="bring-text-btn" onClick={resetSectionOrder}>
                {t("resetOrder")}
              </button>
            </Show>
            <OrderDoneButton onClose={props.onClose} />
          </header>
        </Show>
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
                    class={props.wide ? "pg-icon-btn" : "shop-order-btn"}
                    data-move={`${sec.id}:-1`}
                    aria-label={t("moveUp", { name: sec.name })}
                    disabled={i() === 0}
                    onClick={() => move(sec.id, -1)}
                  >
                    <span class="shop-arrow shop-arrow--up" aria-hidden="true" />
                  </button>
                  <button
                    type="button"
                    class={props.wide ? "pg-icon-btn" : "shop-order-btn"}
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
      </Frame>
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
