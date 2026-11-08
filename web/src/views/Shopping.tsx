import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { createStore } from "solid-js/store";
import { A } from "@solidjs/router";
import { catalogName, itemKey, LANGS, OTHER_SECTION, type CatalogItem, type Lang } from "../lib/shoppingCatalog";
import { LANG_LABEL } from "../lib/shoppingI18n";
import { THEME_ICON, type ShopItem } from "../lib/shoppingOps";
import { readJSON, writeJSON } from "../lib/storage";
import {
  addCatalogItem,
  clearRecent,
  dismissShoppingError,
  dismissUndo,
  initShopping,
  itemIcon,
  itemLang,
  itemSection,
  onOnline,
  orderedSections,
  refreshing,
  refreshShopping,
  removeItems,
  setGrouped,
  setItemLang,
  setLang,
  shop,
  syncing,
  t,
  tapItem,
  undoLast,
} from "../state/shopping";
import "../styles/shopping.css";
import { Button, Card, EmptyState, ErrorState, LoadingState, Page, PageGrid, PageHeader, useWide } from "../components/page";
import { t as ui } from "../i18n";
import { DetailsSheet } from "./shopping/DetailsSheet";
import { withFlip } from "./shopping/flip";
import { SectionIcon } from "./shopping/ItemIcon";
import { ListChooser } from "./shopping/ListChooser";
import { createShopSearch, SearchField, SearchResults } from "./shopping/SearchBar";
import { SectionOrderSheet } from "./shopping/SectionOrderSheet";
import { onTileKeyDown, Tile } from "./shopping/Tile";

const EXPANDED_KEY = "secretary.shopping.expanded";
const RECENT_SECTION = "__recent";
const RESULTS_ID = "shop-results";

interface Group {
  id: string;
  name: string;
  items: ShopItem[];
}

function loadExpanded(): Record<string, boolean> {
  const raw = readJSON("local", EXPANDED_KEY);
  const out: Record<string, boolean> = { [RECENT_SECTION]: true };
  if (raw && typeof raw === "object" && !Array.isArray(raw)) {
    for (const [k, v] of Object.entries(raw)) if (typeof v === "boolean") out[k] = v;
  }
  return out;
}

/** Header of a collapsible section: chevron, optional icon, title, a count, and an optional ⋯ menu. */
function SectionHeader(props: {
  id: string;
  title: string;
  open: boolean;
  onToggle: () => void;
  icon?: { id: string; icon: string };
  count?: number;
  countLabel?: string;
  onMore?: () => void;
  moreOpen?: boolean;
}) {
  return (
    <div class="bring-section-head shop-section-head">
      <button
        type="button"
        class="bring-section-toggle"
        aria-expanded={props.open}
        aria-controls={`bring-sec-${props.id}`}
        aria-label={props.count ? `${props.title}, ${props.countLabel}` : undefined}
        onClick={() => props.onToggle()}
      >
        <span class="bring-section-chevron" classList={{ "bring-section-chevron--open": props.open }} aria-hidden="true" />
        <Show when={props.icon}>
          {(ic) => (
            <span class="shop-section-icon" aria-hidden="true">
              <SectionIcon id={ic().id} icon={ic().icon} />
            </span>
          )}
        </Show>
        <span class="bring-section-title">{props.title}</span>
        <Show when={props.count}>
          {(n) => (
            <span class="shop-count" aria-hidden="true">
              {n()}
            </span>
          )}
        </Show>
      </button>
      <Show when={props.onMore}>
        <button
          type="button"
          class="bring-more"
          aria-label={t("more", { name: props.title })}
          aria-haspopup="menu"
          aria-expanded={!!props.moreOpen}
          onClick={() => props.onMore?.()}
        >
          <span aria-hidden="true">•••</span>
        </button>
      </Show>
    </div>
  );
}

export default function ShoppingView() {
  const { wide, track } = useWide();
  const [detailsId, setDetailsId] = createSignal<string | null>(null);
  const [chooserOpen, setChooserOpen] = createSignal(false);
  const [orderOpen, setOrderOpen] = createSignal(false);
  const [menuOpen, setMenuOpen] = createSignal(false);
  const [recentMenu, setRecentMenu] = createSignal(false);
  const [editing, setEditing] = createSignal(false);
  const [selected, setSelected] = createStore<Record<string, boolean>>({});
  const [expanded, setExpanded] = createStore<Record<string, boolean>>(loadExpanded());
  const search = createShopSearch();
  let root: HTMLDivElement | undefined;
  let board: HTMLDivElement | undefined;
  let listsBtn: HTMLButtonElement | undefined;

  onMount(() => {
    initShopping();
    const onFocus = () => refreshShopping({ ifOlderThanMs: 2_000 });
    const onVisible = () => {
      if (document.visibilityState === "visible") onFocus();
    };
    window.addEventListener("focus", onFocus);
    window.addEventListener("online", onOnline);
    document.addEventListener("visibilitychange", onVisible);
    onCleanup(() => {
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("online", onOnline);
      document.removeEventListener("visibilitychange", onVisible);
    });
  });

  const currentList = createMemo(() => shop.lists.find((l) => l.id === shop.currentListId));
  const listName = () => currentList()?.name ?? "";
  const theme = () => currentList()?.theme ?? "home";
  const recent = createMemo(() =>
    shop.items.filter((i) => i.status === "recent").sort((a, b) => b.updatedAt.localeCompare(a.updatedAt)),
  );
  const selectedIds = createMemo(() => recent().filter((i) => selected[i.id]).map((i) => i.id));

  /** Rank of each section in the list's store-walk order; unknown sections walk last. */
  const sectionRank = createMemo(() => new Map(orderedSections().map((s, i) => [s.id, i])));
  const rankOf = (id: string) => sectionRank().get(id) ?? Number.MAX_SAFE_INTEGER;
  const sectionOf = (item: ShopItem) => {
    const id = itemSection(item);
    return sectionRank().has(id) ? id : OTHER_SECTION;
  };

  /** To buy, in the order of a walk through the store (Bring!'s single red grid). */
  const needed = createMemo(() =>
    shop.items
      .filter((i) => i.status === "needed")
      .map((item, i) => ({ item, rank: rankOf(sectionOf(item)), i }))
      .sort((a, b) => a.rank - b.rank || a.i - b.i)
      .map((x) => x.item),
  );

  /** The same tiles with a header per section, when the menu turns headers on. */
  const groups = createMemo<Group[]>(() => {
    const out: Group[] = [];
    for (const item of needed()) {
      const id = sectionOf(item);
      let g = out[out.length - 1];
      if (!g || g.id !== id) {
        const s = shop.catalog.sections.find((x) => x.id === id);
        g = { id, name: s?.name ?? t("otherSection"), items: [] };
        out.push(g);
      }
      g.items.push(item);
    }
    return out;
  });

  /** Catalog items per section, in store-walk order, for the collapsible catalogue. */
  const catalogSections = createMemo(() => {
    const bySection = new Map<string, CatalogItem[]>();
    for (const item of shop.catalog.items) {
      const id = item.section ?? OTHER_SECTION;
      const list = bySection.get(id) ?? [];
      list.push(item);
      bySection.set(id, list);
    }
    return orderedSections()
      .filter((s) => bySection.has(s.id))
      .map((s) => ({ ...s, items: bySection.get(s.id) ?? [] }));
  });
  const rowByKey = createMemo(() => new Map(shop.items.map((i) => [itemKey(i.catalogId, i.name), i])));
  /** How many to-buy tiles each catalog section has (shown on its header). */
  const neededBySection = createMemo(() => {
    const m = new Map<string, number>();
    for (const i of needed()) m.set(sectionOf(i), (m.get(sectionOf(i)) ?? 0) + 1);
    return m;
  });

  const act = (fn: () => void) => withFlip(root, fn);

  function toggleSection(id: string) {
    setExpanded(id, !expanded[id]);
    writeJSON("local", EXPANDED_KEY, { ...expanded });
  }

  function clearSelection() {
    setSelected(Object.fromEntries(Object.keys(selected).map((k) => [k, false])));
  }

  function startEditing() {
    setRecentMenu(false);
    clearSelection();
    setEditing(true);
  }

  function stopEditing() {
    setEditing(false);
    clearSelection();
  }

  function itemLabel(item: ShopItem): string {
    const name = item.spec ? `${item.name} (${item.spec})` : item.name;
    if (editing() && item.status === "recent") return t(selected[item.id] ? "selectedRecent" : "selectRecent", { name });
    return t(item.status === "needed" ? "toBuyItem" : "recentItem", { name });
  }

  function specOf(item: ShopItem): string {
    const qty = item.quantity !== null ? `${item.quantity}${item.unit ? ` ${item.unit}` : ""}` : "";
    return [qty, item.spec].filter(Boolean).join(" · ");
  }

  const ItemTile = (p: { item: ShopItem }) => {
    const isEditing = () => editing() && p.item.status === "recent";
    return (
      <Tile
        flipId={p.item.id}
        kind={p.item.status === "needed" ? "need" : "recent"}
        icon={itemIcon(p.item)}
        iconPicked={p.item.iconPicked}
        catalogId={p.item.catalogId}
        section={p.item.section}
        name={p.item.name}
        spec={specOf(p.item)}
        label={itemLabel(p.item)}
        checked={isEditing() ? !!selected[p.item.id] : undefined}
        onTap={() => (isEditing() ? setSelected(p.item.id, !selected[p.item.id]) : act(() => tapItem(p.item.id)))}
        onDetails={() => setDetailsId(p.item.id)}
        detailsLabel={t("itemOptions", { name: p.item.name })}
      />
    );
  };

  /** The ⋯ menu of the list: section order, headers, languages, refresh. */
  const ListMenu = () => (
    <>
      <Show when={menuOpen()}>
        <div class="bring-menu shop-menu" role="menu" aria-label={t("settings")} onKeyDown={(e) => e.key === "Escape" && setMenuOpen(false)}>
          <button
            type="button"
            class="bring-menu-row"
            role="menuitem"
            onClick={() => {
              setMenuOpen(false);
              setOrderOpen(true);
            }}
          >
            {t("sortSections")}
          </button>
          <label class="bring-menu-row">
            <input type="checkbox" checked={shop.grouped} onChange={(e) => setGrouped(e.currentTarget.checked)} />
            <span>{t("groupToggle")}</span>
          </label>
          <label class="bring-menu-row">
            <span>{t("itemNames")}</span>
            <select
              class="bring-menu-select"
              value={shop.itemLangPref ?? ""}
              onChange={(e) => setItemLang((e.currentTarget.value || null) as Lang | null)}
            >
              <option value="">
                {t("itemNamesAuto")} ({LANG_LABEL[itemLang()]})
              </option>
              <For each={LANGS}>{(l) => <option value={l}>{LANG_LABEL[l]}</option>}</For>
            </select>
          </label>
          <label class="bring-menu-row">
            <span>{t("language")}</span>
            <select class="bring-menu-select" value={shop.lang} onChange={(e) => setLang(e.currentTarget.value as Lang)}>
              <For each={LANGS}>{(l) => <option value={l}>{LANG_LABEL[l]}</option>}</For>
            </select>
          </label>
          <button
            type="button"
            class="bring-menu-row"
            role="menuitem"
            disabled={refreshing() || syncing()}
            onClick={() => {
              setMenuOpen(false);
              refreshShopping();
            }}
          >
            {t("refresh")}
          </button>
        </div>
      </Show>
    </>
  );

  /** The list switcher: the big themed button on a phone, a quiet button in the desktop header. */
  const ListButton = () => (
    <button
      ref={listsBtn}
      type="button"
      class={wide() ? "pg-btn" : "shop-list-btn"}
      aria-label={t("switchList", { name: listName() })}
      aria-haspopup="dialog"
      onClick={() => setChooserOpen(true)}
    >
      <Show when={!wide()}>
        <span class="shop-list-art" aria-hidden="true">
          {THEME_ICON[theme()]}
        </span>
      </Show>
      <span class="shop-list-name">{listName()}</span>
      <Show when={!wide()}>
        <span class="bring-badge shop-badge" role="status" aria-label={t("toBuyCount", { n: needed().length })}>
          {needed().length}
        </span>
      </Show>
      <span class="shop-caret" aria-hidden="true" />
    </button>
  );

  /** The to-buy tiles (one grid, or one per section when headers are on), or the empty state. */
  const ToBuy = () => (
    <section class="shop-tobuy" aria-label={wide() ? undefined : t("toBuy")}>
      <Show
        when={needed().length > 0}
        fallback={
          <Show
            when={wide()}
            fallback={
              <div class="bring-empty shop-empty">
                <span class="bring-empty-art" aria-hidden="true">
                  🧺
                </span>
                <div>
                  <p class="bring-empty-title">{t("emptyTitle")}</p>
                  <p class="bring-empty-hint">{t("emptyHint")}</p>
                </div>
              </div>
            }
          >
            <EmptyState title={t("emptyTitle")} text={t("emptyHint")} />
          </Show>
        }
      >
        <Show
          when={shop.grouped}
          fallback={
            <div class="shop-grid" role="list" aria-label={t("toBuy")}>
              <For each={needed()}>{(item) => <ItemTile item={item} />}</For>
            </div>
          }
        >
          <For each={groups()}>
            {(g) => (
              <>
                <h2 class="shop-group-title">{g.name}</h2>
                <div class="shop-grid" role="list" aria-label={g.name}>
                  <For each={g.items}>{(item) => <ItemTile item={item} />}</For>
                </div>
              </>
            )}
          </For>
        </Show>
      </Show>
    </section>
  );

  /** Recently used: a collapsible section with its ⋯ menu and the edit bar. */
  const Recent = () => (
    <Show when={recent().length > 0}>
      <section class="bring-section shop-section shop-recent">
        <SectionHeader
          id={RECENT_SECTION}
          title={t("recently")}
          open={!!expanded[RECENT_SECTION] || editing()}
          onToggle={() => toggleSection(RECENT_SECTION)}
          onMore={() => setRecentMenu((v) => !v)}
          moreOpen={recentMenu()}
        />
        <Show when={recentMenu()}>
          <div class="bring-menu bring-menu--section" role="menu" aria-label={t("recently")}>
            <button type="button" class="bring-menu-row" role="menuitem" onClick={startEditing}>
              {t("editRecently")}
            </button>
            <button
              type="button"
              class="bring-menu-row"
              role="menuitem"
              onClick={() => {
                setRecentMenu(false);
                act(clearRecent);
              }}
            >
              {t("clearRecently")}
            </button>
          </div>
        </Show>
        <Show when={editing()}>
          <div class="bring-edit-bar">
            <button type="button" class={wide() ? "pg-btn" : "bring-btn bring-btn--secondary bring-btn--small"} onClick={stopEditing}>
              {t("cancel")}
            </button>
            <button
              type="button"
              class={wide() ? "pg-btn" : "bring-btn bring-btn--secondary bring-btn--small"}
              disabled={selectedIds().length === 0}
              onClick={() => {
                const ids = selectedIds();
                act(() => removeItems(ids));
                stopEditing();
              }}
            >
              {selectedIds().length ? t("removeN", { n: selectedIds().length }) : t("remove")}
            </button>
          </div>
        </Show>
        <Show when={expanded[RECENT_SECTION] || editing()}>
          <div class="shop-grid" id={`bring-sec-${RECENT_SECTION}`} role="list" aria-label={t("recently")}>
            <For each={recent()}>{(item) => <ItemTile item={item} />}</For>
          </div>
        </Show>
      </section>
    </Show>
  );

  /** The catalogue sections in store-walk order. */
  const CatalogSections = () => (
    <For each={catalogSections()}>
      {(sec) => (
        <section class="bring-section shop-section">
          <SectionHeader
            id={sec.id}
            title={sec.name}
            icon={{ id: sec.id, icon: sec.icon }}
            count={neededBySection().get(sec.id)}
            countLabel={t("onListCount", { n: neededBySection().get(sec.id) ?? 0 })}
            open={!!expanded[sec.id]}
            onToggle={() => toggleSection(sec.id)}
          />
          <Show when={expanded[sec.id]}>
            <div class="shop-grid" id={`bring-sec-${sec.id}`} role="list" aria-label={sec.name}>
              <For each={sec.items}>
                {(cat) => {
                  const name = () => catalogName(cat, itemLang());
                  const row = () => rowByKey().get(itemKey(cat.id, name()));
                  const onList = () => row()?.status === "needed";
                  return (
                    <Tile
                      flipId={`catalog:${cat.id}`}
                      kind="catalog"
                      icon={cat.icon}
                      catalogId={cat.id}
                      name={name()}
                      spec={onList() ? row()?.spec : undefined}
                      selected={onList()}
                      label={onList() ? t("toBuyItem", { name: name() }) : t("catalogItem", { name: name() })}
                      onTap={() => {
                        const r = row();
                        act(() => (r?.status === "needed" ? tapItem(r.id) : void addCatalogItem(cat)));
                      }}
                    />
                  );
                }}
              </For>
            </div>
          </Show>
        </section>
      )}
    </For>
  );

  const PhoneLayout = () => (
    <>
            <div class="bring-scroll shop-scroll">
              <header class={`shop-head bring-theme--${theme()}`}>
                <div class="shop-bar">
                  <h1 class="shop-title">
      <ListButton />
                  </h1>
                  <div class="shop-bar-end">
                    <Show when={shop.pending > 0 || !shop.online}>
                      <span class="bring-sync" classList={{ "bring-sync--offline": !shop.online }} role="status">
                        {shop.online ? t("syncing") : t("offline", { n: shop.pending })}
                      </span>
                    </Show>
                    <A href="/shopping/map" class="shop-icon-btn" aria-label={t("mapBtn")} title={t("mapBtn")}>
                      <span class="shop-map-glyph" aria-hidden="true" />
                    </A>
                    <button
                      type="button"
                      class="shop-icon-btn"
                      aria-label={t("settings")}
                      aria-haspopup="menu"
                      aria-expanded={menuOpen()}
                      onClick={() => setMenuOpen((v) => !v)}
                    >
                      <span aria-hidden="true">•••</span>
                    </button>
                  </div>
      <ListMenu />
                </div>
                <SearchField search={search} act={act} resultsId={RESULTS_ID} />
              </header>

              <Show when={shop.actionError}>
                {(msg) => (
                  <div class="bring-notice" role="alert">
                    <span>{msg()}</span>
                    <button type="button" class="bring-notice-close" onClick={dismissShoppingError} aria-label={t("close")}>
                      ×
                    </button>
                  </div>
                )}
              </Show>

              <div
                class="bring-board shop-board"
                ref={board}
                onKeyDown={(e) => onTileKeyDown(e, (search.active() && document.getElementById(RESULTS_ID)) || board)}
              >
                <Show
                  when={shop.loaded}
                  fallback={
                    <Show
                      when={shop.loadError}
                      fallback={
                        <p class="bring-muted bring-center" role="status">
                          {t("loading")}
                        </p>
                      }
                    >
                      {(msg) => (
                        <div class="bring-empty" role="alert">
                          <p class="bring-empty-title">{msg()}</p>
                          <button type="button" class="bring-btn bring-btn--secondary" onClick={() => refreshShopping()}>
                            {t("retry")}
                          </button>
                        </div>
                      )}
                    </Show>
                  }
                >
                  <Show when={shop.loadError}>
                    {(msg) => (
                      <p class="bring-muted shop-pad" role="status">
                        {msg()} {t("showingCached")}
                      </p>
                    )}
                  </Show>

                  <Show when={search.active()}>
                    <SearchResults search={search} act={act} id={RESULTS_ID} />
                  </Show>

                  <div class="shop-home" hidden={search.active()}>
                    {/* To buy */}
                    <ToBuy />
                    {/* Recently used */}
                    <Recent />
                    {/* Catalogue, in store-walk order */}
                    <Show when={catalogSections().length > 0}>
                      <div class="shop-cat-head">
                        <h2 class="shop-heading">{t("catalog")}</h2>
                        <button type="button" class="shop-link-btn" onClick={() => setOrderOpen(true)}>
                          {t("sortSections")}
                        </button>
                      </div>
                    </Show>
                    <CatalogSections />
                  </div>
                </Show>
              </div>
            </div>
    </>
  );

  /** The desktop frame: page header, search and tiles in the main column, recently used and offers in the rail. */
  const WideLayout = () => (
    <div class="bring-scroll shopping-scroll">
      <Page class="shopping-wide">
        <PageHeader
          title={ui("shell.navShopping")}
          context={
            <>
              {listName()} · {t("toBuyCount", { n: needed().length })}
              <Show when={shop.pending > 0 || !shop.online}>
                {" · "}
                <span role="status">{shop.online ? t("syncing") : t("offline", { n: shop.pending })}</span>
              </Show>
              <Show when={shop.loaded && shop.loadError}> · {t("showingCached")}</Show>
            </>
          }
          action={
            <div class="shopping-head-tools">
              <ListButton />
              <Button aria-label={t("settings")} aria-haspopup="menu" aria-expanded={menuOpen()} onClick={() => setMenuOpen((v) => !v)}>
                •••
              </Button>
              <ListMenu />
            </div>
          }
        />
        <Show when={shop.actionError}>
          {(msg) => (
            <div class="pg-notice" role="alert">
              <span>{msg()}</span>
              <Button variant="ghost" onClick={dismissShoppingError} aria-label={t("close")}>
                ×
              </Button>
            </div>
          )}
        </Show>
        <div ref={board} onKeyDown={(e) => onTileKeyDown(e, (search.active() && document.getElementById(RESULTS_ID)) || board)}>
          <PageGrid
            layout="rail"
            railLabel={t("recently")}
            rail={
              <>
                <Show when={shop.loaded && recent().length > 0}>
                  <Card label={t("recently")} class="shopping-recent">
                    <Recent />
                  </Card>
                </Show>
                <Card title={t("mapTitle")}>
                  <A class="pg-link" href="/shopping/map">
                    {t("mapBtn")}
                  </A>
                </Card>
              </>
            }
          >
            <SearchField search={search} act={act} resultsId={RESULTS_ID} wide />
            <Show
              when={shop.loaded}
              fallback={
                <Show when={shop.loadError} fallback={<LoadingState label={t("loading")} />}>
                  {(msg) => <ErrorState message={msg()} retryLabel={t("retry")} onRetry={() => refreshShopping()} />}
                </Show>
              }
            >
              <Show
                when={search.active()}
                fallback={
                  <>
                    <Card title={t("toBuy")}>
                      <ToBuy />
                    </Card>
                    <Show when={catalogSections().length > 0}>
                      <Card
                        title={t("catalog")}
                        actions={
                          <Button variant="ghost" onClick={() => setOrderOpen(true)}>
                            {t("sortSections")}
                          </Button>
                        }
                      >
                        <CatalogSections />
                      </Card>
                    </Show>
                  </>
                }
              >
                <Card label={t("suggestions")}>
                  <SearchResults search={search} act={act} id={RESULTS_ID} />
                </Card>
              </Show>
            </Show>
          </PageGrid>
        </div>
      </Page>
    </div>
  );

  return (
    <div
      class="bring shop"
      ref={(el) => {
        root = el;
        track(el);
      }}
      lang={shop.lang}
    >
      <Show when={wide()} fallback={<PhoneLayout />}>
        <WideLayout />
      </Show>

      <div class="bring-snackbar-region" role="status" aria-live="polite">
        <Show when={shop.undo}>
          {(u) => (
            <div class="bring-snackbar">
              <span class="bring-snackbar-msg">{u().message}</span>
              <Show when={u().inverse}>
                <button type="button" class="bring-snackbar-action" onClick={() => act(undoLast)}>
                  {t("undo")}
                </button>
              </Show>
              <button type="button" class="bring-snackbar-close" aria-label={t("close")} onClick={dismissUndo}>
                ×
              </button>
            </div>
          )}
        </Show>
      </div>

      <DetailsSheet wide={wide()} itemId={detailsId()} onClose={() => setDetailsId(null)} />
      <SectionOrderSheet wide={wide()} open={orderOpen()} onClose={() => setOrderOpen(false)} />
      <ListChooser
        wide={wide()}
        open={chooserOpen()}
        onClose={() => {
          setChooserOpen(false);
          queueMicrotask(() => listsBtn?.focus());
        }}
      />
    </div>
  );
}
