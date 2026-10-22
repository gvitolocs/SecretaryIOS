import { createMemo, createSignal, For, Show } from "solid-js";
import { catalogName, exactMatch, itemKey, searchCatalog, type CatalogItem } from "../../lib/shoppingCatalog";
import type { ShopItem } from "../../lib/shoppingOps";
import { addCatalogItem, addText, catalogIndex, itemIcon, rowKeyId, shop, t, tapItem } from "../../state/shopping";
import { onTileKeyDown, Tile } from "./Tile";

/**
 * The search bottom sheet (Bring's `fragment_item_search`): it peeks at the
 * bottom with a handle, the search field and a round add button. Focusing the
 * field expands it upwards; results are tiles in the same 3-column grid, and
 * "Done" collapses it again.
 */
export function SearchSheet(props: { act: (fn: () => void) => void }) {
  const [query, setQuery] = createSignal("");
  const [expanded, setExpanded] = createSignal(false);
  let input: HTMLInputElement | undefined;
  let grid: HTMLDivElement | undefined;

  const rowByKey = createMemo(() => new Map(shop.items.map((i) => [itemKey(rowKeyId(i), i.name), i])));
  const recent = createMemo(() =>
    shop.items.filter((i) => i.status === "recent").sort((a, b) => b.updatedAt.localeCompare(a.updatedAt)),
  );
  const results = createMemo<CatalogItem[]>(() => {
    const q = query().trim();
    if (!q) return [];
    const boost = new Map(recent().map((r, i) => [r.catalogId ?? "", 100 - i]));
    return searchCatalog(catalogIndex(), q, { limit: 30, boost: (id) => boost.get(id) ?? 0 });
  });
  const showCustom = createMemo(() => {
    const q = query().trim();
    return q !== "" && !exactMatch(catalogIndex(), q);
  });

  const rowFor = (cat: CatalogItem): ShopItem | undefined => rowByKey().get(itemKey(cat.id, catalogName(cat, shop.lang)));

  function expand() {
    setExpanded(true);
  }

  function done() {
    setQuery("");
    setExpanded(false);
    input?.blur();
  }

  function addTyped() {
    const q = query().trim();
    if (!q) {
      input?.focus();
      return;
    }
    props.act(() => void addText(q));
    setQuery("");
    input?.focus();
  }

  return (
    <div class="bring-search" classList={{ "bring-search--open": expanded() }}>
      <button
        type="button"
        class="bring-sheet-handle"
        aria-label={expanded() ? t("close") : t("searchLabel")}
        aria-expanded={expanded()}
        onClick={() => (expanded() ? done() : input?.focus())}
      >
        <span aria-hidden="true" />
      </button>
      <form
        class="bring-search-row"
        role="search"
        onSubmit={(e) => {
          e.preventDefault();
          addTyped();
        }}
      >
        <label class="sr-only" for="bring-search-field">
          {t("searchLabel")}
        </label>
        <input
          ref={input}
          id="bring-search-field"
          class="bring-search-field"
          type="search"
          placeholder={t("searchPlaceholder")}
          value={query()}
          onFocus={expand}
          onClick={expand}
          onInput={(e) => {
            setQuery(e.currentTarget.value);
            expand();
          }}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              done();
            } else if (e.key === "ArrowDown") {
              const first = grid?.querySelector<HTMLElement>("[data-tile]");
              if (first) {
                e.preventDefault();
                first.focus();
              }
            }
          }}
          enterkeyhint="done"
          autocomplete="off"
          maxlength="200"
          aria-controls="bring-search-results"
        />
        <Show
          when={expanded()}
          fallback={
            <button type="submit" class="bring-search-add" aria-label={t("addTyped")}>
              <span aria-hidden="true" />
            </button>
          }
        >
          <button type="button" class="bring-search-done" onClick={done}>
            {t("done")}
          </button>
        </Show>
      </form>

      <Show when={expanded()}>
        <div
          class="bring-search-results"
          id="bring-search-results"
          ref={grid}
          onKeyDown={(e) => {
            if (e.key === "Escape") done();
            else onTileKeyDown(e, grid);
          }}
        >
          <Show
            when={query().trim()}
            fallback={
              <Show when={recent().length > 0}>
                <h2 class="bring-group-title">{t("recently")}</h2>
                <div class="bring-grid" role="list" aria-label={t("recently")}>
                  <For each={recent()}>
                    {(item) => (
                      <Tile
                        flipId={`search:${item.id}`}
                        kind="recent"
                        icon={itemIcon(item)}
                        name={item.name}
                        spec={item.spec}
                        label={t("recentItem", { name: item.name })}
                        onTap={() => props.act(() => tapItem(item.id))}
                      />
                    )}
                  </For>
                </div>
              </Show>
            }
          >
            <div class="bring-grid" role="list" aria-label={t("searchLabel")}>
              <Show when={showCustom()}>
                <Tile
                  flipId={`custom:${query().trim()}`}
                  kind="custom"
                  icon={null}
                  name={query().trim()}
                  label={t("addCustom", { name: query().trim() })}
                  onTap={addTyped}
                />
              </Show>
              <For each={results()}>
                {(cat) => {
                  const row = () => rowFor(cat);
                  const name = () => catalogName(cat, shop.lang);
                  const onList = () => row()?.status === "needed";
                  return (
                    <Tile
                      flipId={`cat:${cat.id}`}
                      kind="catalog"
                      icon={cat.icon}
                      name={name()}
                      spec={onList() ? row()?.spec : undefined}
                      selected={onList()}
                      label={onList() ? t("toBuyItem", { name: name() }) : t("catalogItem", { name: name() })}
                      onTap={() => {
                        const r = row();
                        props.act(() => (r?.status === "needed" ? tapItem(r.id) : void addCatalogItem(cat)));
                      }}
                    />
                  );
                }}
              </For>
            </div>
            <Show when={results().length === 0 && !showCustom()}>
              <p class="bring-muted">{t("noResults")}</p>
            </Show>
          </Show>
        </div>
      </Show>
    </div>
  );
}
