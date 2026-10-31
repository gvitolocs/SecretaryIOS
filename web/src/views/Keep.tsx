import { useLocation, useNavigate } from "@solidjs/router";
import { createEffect, createMemo, createSignal, on, onCleanup, onMount, Show, type JSX } from "solid-js";
import type { KeepNote } from "../api/keep";
import { AccountDialog } from "../components/AccountDialog";
import { EmptyState as PageEmptyState, LoadingState, Page, PageGrid, PageHeader, useWide } from "../components/page";
import { KeepContext, KeepRootContext, type KeepCtx } from "../components/keep/ctx";
import { Composer } from "../components/keep/Composer";
import { ConfirmDialog, LabelsDialog } from "../components/keep/Dialogs";
import { ArchiveIcon, BellIcon, BulbOutlineIcon, CloseIcon, LabelIcon, SearchIcon, TrashIcon } from "../components/keep/icons";
import { KeepNavChips, KeepRailNav } from "../components/keep/KeepNav";
import { Masonry } from "../components/keep/Masonry";
import { NoteCard } from "../components/keep/NoteCard";
import { NoteDialog } from "../components/keep/NoteDialog";
import { SearchPage, typeLabel } from "../components/keep/SearchPage";
import { colorName } from "../components/keep/Pickers";
import { swatchFill } from "../lib/keepColors";
import { SelectionBar } from "../components/keep/SelectionBar";
import { Snackbar } from "../components/keep/Snackbar";
import { GridViewIcon, ListViewIcon, RefreshIcon, SettingsIcon } from "../components/keep/icons";
import { moveItem } from "../components/keep/ChecklistEditor";
import { t } from "../i18n";
import { positionBetween, searchActive, searchNotes, splitPinned, viewNotes, emptySearch, type KeepView, type SearchState } from "../lib/keepFilter";
import { keepHref, noteHref, parseKeepPath } from "../lib/keepRoute";
import { keep, startAutoSync } from "../state/keep";
import { clearSelection, listMode, search, searchOpen, selection, setDrawerOpen, setListMode, setSearch, setSearchOpen } from "../state/keepUi";
import "../styles/keep.css";

// Keep: the owner's notes at /keep, moved here from keep.google.com, on the Lucina page layout at every width
// (owner, 2026-10-31: "completely migrate the layout to be Lucina"): the page header with the search field, the
// shared chips or rail for the sections, Lucina's navy and accent. Every Keep feature stays.
// Routes: /keep, /keep/reminders, /keep/archive, /keep/trash, /keep/label/<name>, /keep/note/<id> (a note's dialog).
// The cache, sync and optimistic writes live in state/keep.ts; everything on this page reads that store.

function keepSectionContext(view: KeepView, labelName: (tag: string) => string): string {
  switch (view.kind) {
    case "reminders":
      return t("keep.reminders");
    case "archive":
      return t("keep.archive");
    case "trash":
      return t("keep.trash");
    case "label":
      return labelName((view as { tag: string }).tag);
    default:
      return t("keep.notes");
  }
}

export default function KeepView() {
  const location = useLocation();
  const navigate = useNavigate();
  const [rootEl, setRootEl] = createSignal<HTMLElement>();
  const [labelsOpen, setLabelsOpen] = createSignal(false);
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [confirmEmpty, setConfirmEmpty] = createSignal(false);

  const route = createMemo(() => parseKeepPath(location.pathname, location.search));
  const view = (): KeepView => route().view;
  // Lucina's shell is dark whatever the device's theme; Keep follows it (its note colours use the dark palette).
  const theme = () => "dark" as const;
  // The field updates at once; the notes follow 150 ms after the last key, so typing never stutters on a long list.
  const [query, setQuery] = createSignal<SearchState>(search());
  createEffect(
    on(search, (s) => {
      if (!s.q.trim() || s.q === query().q) return setQuery(s);
      const timer = setTimeout(() => setQuery(s), 150);
      onCleanup(() => clearTimeout(timer));
    }),
  );
  const searching = createMemo(() => searchActive(query()));

  onMount(() => {
    void keep.load();
    onCleanup(startAutoSync(keep));
  });

  // A deep link may carry a slug (Lucina cites notes as [note:<slug>]): use the note's id from then on,
  // asking the server for a note this device has not synced yet.
  const dialogId = createMemo(() => {
    const key = route().noteId;
    return key ? (keep.findByKey(key)?.id ?? key) : undefined;
  });
  createEffect(() => {
    const key = route().noteId;
    if (!key || !keep.loaded()) return;
    const found = keep.findByKey(key);
    if (found && found.id !== key) {
      navigate(noteHref(found.id, view(), labelDisplay()), { replace: true });
    } else if (!found && keep.status() !== "syncing") void keep.ensure(key);
  });

  // Moving to another view drops the selection and the search.
  createEffect(
    on(
      () => keepHref(view(), view().kind === "label" ? keep.labelName((view() as { tag: string }).tag) : undefined),
      () => clearSelection(),
      { defer: true },
    ),
  );
  // A selection of notes that left the cache (deleted, synced away) shrinks with it.
  const selectedNotes = createMemo<KeepNote[]>(() => {
    const sel = selection();
    return keep.all().filter((n) => sel.has(n.id));
  });

  const base = () => keepHref(view(), view().kind === "label" ? keep.labelName((view() as { tag: string }).tag) : undefined);
  const labelDisplay = () => (view().kind === "label" ? keep.labelName((view() as { tag: string }).tag) : undefined);

  const ctx: KeepCtx = {
    view,
    theme,
    open: (id) => navigate(noteHref(id, view(), labelDisplay())),
    goLabel: (tag) => go(keepHref({ kind: "label", tag }, keep.labelName(tag))),
  };

  function go(href: string) {
    setSearch(emptySearch());
    setSearchOpen(false);
    setDrawerOpen(false);
    clearSelection();
    navigate(href);
  }

  function closeNote() {
    const r = route();
    const here = location.pathname;
    if (here.startsWith("/keep/note/")) navigate(base(), { replace: true });
    else if (r.noteId) navigate(base(), { replace: true });
  }

  // Escape clears a selection (dialogs and popovers handle their own).
  onMount(() => {
    const key = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented || route().noteId || labelsOpen() || settingsOpen() || confirmEmpty()) return;
      if (selection().size) clearSelection();
    };
    document.addEventListener("keydown", key);
    onCleanup(() => document.removeEventListener("keydown", key));
  });

  // ---------------------------------------------------------------- what the page shows

  const shown = createMemo<KeepNote[]>(() => {
    const all = keep.all();
    if (searching()) return searchNotes(all, query(), keep.labelName);
    return viewNotes(all, view());
  });
  const sections = createMemo(() => {
    const list = shown();
    // Search results are one list, best match first: a pinned note shows only when it matches, in its rank.
    if (searching() || view().kind === "trash") return { pinned: [] as KeepNote[], others: list, reminders: false as const };
    if (view().kind === "reminders" && !searching()) {
      const now = Date.now();
      const upcoming = list.filter((n) => Date.parse(n.reminder!.at) >= now);
      const earlier = list.filter((n) => Date.parse(n.reminder!.at) < now);
      return { pinned: upcoming, others: earlier, reminders: true as const };
    }
    return { ...splitPinned(list), reminders: false as const };
  });
  const canReorder = () => !searching() && ["notes", "archive", "label"].includes(view().kind) && selection().size === 0 && typeof matchMedia === "function" && matchMedia("(pointer: fine)").matches;

  function reorder(section: KeepNote[], dragged: string, target: string) {
    const from = section.findIndex((n) => n.id === dragged);
    const to = section.findIndex((n) => n.id === target);
    if (from < 0 || to < 0) return;
    // Dropping on a card puts the dragged note where that card is.
    const order = moveItem(section, from, from < to ? to + 1 : to);
    const at = order.findIndex((n) => n.id === dragged);
    void keep.update(dragged, { position: positionBetween(order[at - 1]?.position, order[at + 1]?.position) });
  }

  const Section = (p: { notes: KeepNote[]; heading?: string }) => (
    <Show when={p.notes.length > 0}>
      <Masonry
        items={p.notes}
        list={listMode()}
        heading={p.heading}
        canDrag={canReorder()}
        onDrop={(from, target) => reorder(p.notes, from, target)}
      >
        {(note, drag) => <NoteCard note={note} draggable={drag.draggable} onDragStart={drag.onDragStart} onDragEnd={drag.onDragEnd} />}
      </Masonry>
    </Show>
  );

  const emptyFor = (): { icon: () => JSX.Element; text: string } => {
    if (searching()) return { icon: BulbOutlineIcon, text: t("keep.noMatches") };
    switch (view().kind) {
      case "reminders":
        return { icon: BellIcon, text: t("keep.emptyReminders") };
      case "archive":
        return { icon: ArchiveIcon, text: t("keep.emptyArchive") };
      case "trash":
        return { icon: TrashIcon, text: t("keep.emptyTrash") };
      case "label":
        return { icon: LabelIcon, text: t("keep.emptyLabel") };
      default:
        return { icon: BulbOutlineIcon, text: t("keep.emptyNotes") };
    }
  };

  const trashIds = () => keep.all().filter((n) => n.deletedAt).map((n) => n.id);
  const showFilters = () => searchOpen() && !searching();

  const measured = useWide();
  const wide = measured.wide;

  // The filters picked on the search page (a type, a label, a colour), each removable.
  const filterChips = () => (
    <Show when={search().type || search().label || search().color}>
      <div class="kp-filterchips" role="group" aria-label={t("keep.search")}>
        <Show when={search().type}>
          {(type) => (
            <span class="kp-search-chip" data-chip="type">
              {typeLabel(type())}
              <button type="button" aria-label={t("keep.searchClear")} onClick={() => setSearch({ ...search(), type: undefined })}>
                <CloseIcon />
              </button>
            </span>
          )}
        </Show>
        <Show when={search().label}>
          {(tag) => (
            <span class="kp-search-chip" data-chip="label">
              <LabelIcon />
              {keep.labelName(tag())}
              <button type="button" aria-label={t("keep.searchClear")} onClick={() => setSearch({ ...search(), label: undefined })}>
                <CloseIcon />
              </button>
            </span>
          )}
        </Show>
        <Show when={search().color}>
          {(c) => (
            <span class="kp-search-chip" data-chip="color">
              <span class="kp-search-dot" style={{ "background-color": swatchFill(c(), theme()) }} />
              {colorName(c())}
              <button type="button" aria-label={t("keep.searchClear")} onClick={() => setSearch({ ...search(), color: undefined })}>
                <CloseIcon />
              </button>
            </span>
          )}
        </Show>
      </div>
    </Show>
  );

  // Functions, called inside the providers below: JSX built out here would render its cards outside KeepContext and
  // get the fallback context, whose open() does nothing (a click on a note opened nothing, 2026-10-31).
  const keepMain = () => (
    <>
      {filterChips()}
      <Show when={keep.status() === "error" && keep.error()}>
        <p class="pg-notice" role="status">
          {t("keep.syncFailed")}
        </p>
      </Show>
      <Show when={showFilters()}>
        <SearchPage labels={keep.labels()} theme={theme()} onPick={(p) => setSearch({ ...search(), ...p })} />
      </Show>
      <Show when={!showFilters()}>
        <Show when={!searching() && (view().kind === "notes" || view().kind === "label")}>
          <Composer initialLabel={labelDisplay()} />
        </Show>
        <Show when={view().kind === "trash" && !searching() && trashIds().length > 0}>
          <div class="kp-trash-bar">
            <span>{t("keep.trashHint")}</span>
            <button type="button" class="kp-text-btn kp-text-btn--accent" data-action="empty-trash" onClick={() => setConfirmEmpty(true)}>
              {t("keep.emptyTrashBtn")}
            </button>
          </div>
        </Show>
        <Show
          when={keep.loaded()}
          fallback={
            <LoadingState label={t("common.loading")} />
          }
        >
          <Show
            when={shown().length > 0}
            fallback={
              <PageEmptyState text={emptyFor().text} />
            }
          >
            <Show
              when={(sections() as { reminders?: boolean }).reminders}
              fallback={
                <>
                  <Section notes={sections().pinned} heading={t("keep.pinned")} />
                  <Section notes={sections().others} heading={sections().pinned.length ? t("keep.others") : undefined} />
                </>
              }
            >
              <Section notes={sections().pinned} heading={sections().others.length ? t("keep.upcoming") : undefined} />
              <Section notes={sections().others} heading={sections().pinned.length ? t("keep.earlier") : undefined} />
            </Show>
          </Show>
        </Show>
      </Show>
    </>
  );

  const headerTools = () => (
    <div class="kp-tools-row">
      <label class="kp-searchfield">
        <SearchIcon />
        <input
          class="pg-input"
          type="search"
          autocomplete="off"
          placeholder={t("keep.search")}
          aria-label={t("keep.search")}
          value={search().q}
          onFocus={() => setSearchOpen(true)}
          onInput={(e) => setSearch({ ...search(), q: e.currentTarget.value })}
          onKeyDown={(e) => {
            if (e.key !== "Escape") return;
            setSearch(emptySearch());
            setSearchOpen(false);
            e.currentTarget.blur();
          }}
        />
      </label>
      <Show when={searchOpen() || searching()}>
        <button
          type="button"
          class="pg-icon-btn"
          aria-label={t("keep.searchClear")}
          title={t("keep.searchClear")}
          onClick={() => {
            setSearch(emptySearch());
            setSearchOpen(false);
          }}
        >
          <CloseIcon />
        </button>
      </Show>
      <button
        type="button"
        class="pg-icon-btn"
        classList={{ "kp-spin": keep.status() === "syncing" }}
        aria-label={t("keep.refresh")}
        title={t("keep.refresh")}
        onClick={() => void keep.sync()}
      >
        <RefreshIcon />
      </button>
      <button
        type="button"
        class="pg-icon-btn"
        aria-label={listMode() ? t("keep.gridView") : t("keep.listView")}
        title={listMode() ? t("keep.gridView") : t("keep.listView")}
        onClick={() => setListMode(!listMode())}
      >
        {listMode() ? <GridViewIcon /> : <ListViewIcon />}
      </button>
      <button type="button" class="pg-icon-btn" aria-label={t("keep.settings")} title={t("keep.settings")} onClick={() => setSettingsOpen(true)}>
        <SettingsIcon />
      </button>
    </div>
  );

  const navProps = () => ({
    view: view(),
    labels: keep.labels(),
    labelName: keep.labelName,
    onGo: go,
    onEditLabels: () => setLabelsOpen(true),
  });

  return (
    <KeepRootContext.Provider value={rootEl}>
      <KeepContext.Provider value={ctx}>
        <div
          class="kp kp--lucina"
          ref={(el) => {
            setRootEl(el);
            measured.track(el);
          }}
          classList={{ "kp--desk": wide() }}
          data-theme={theme()}
          data-mode={listMode() ? "list" : "grid"}
          data-selecting={selection().size > 0 ? "yes" : undefined}
          data-searching={searchOpen() ? "yes" : undefined}
        >
          <Show when={selection().size > 0}>
            <SelectionBar notes={selectedNotes()} />
          </Show>
          <Page class="keep-page">
            <PageHeader
              title={t("keep.title")}
              context={searching() ? t("keep.search") : keepSectionContext(view(), keep.labelName)}
              action={headerTools()}
            />
            <Show
              when={wide()}
              fallback={
                <>
                  <KeepNavChips {...navProps()} />
                  <main class="kp-main" id="keep-main" aria-label={t("keep.title")}>
                    {keepMain()}
                  </main>
                </>
              }
            >
              <PageGrid layout="split" railLabel={t("keep.sections")} rail={<KeepRailNav {...navProps()} />}>
                <main class="kp-main keep-desk-main" id="keep-main" aria-label={t("keep.title")}>
                  {keepMain()}
                </main>
              </PageGrid>
            </Show>
          </Page>
          <Show when={dialogId()}>{(id) => <NoteDialog id={id()} onClose={closeNote} />}</Show>
          <Show when={labelsOpen()}>
            <LabelsDialog onClose={() => setLabelsOpen(false)} />
          </Show>
          <Show when={confirmEmpty()}>
            <ConfirmDialog
              title={t("keep.emptyTrashTitle")}
              body={t("keep.emptyTrashBody")}
              confirm={t("keep.emptyTrashBtn")}
              onCancel={() => setConfirmEmpty(false)}
              onConfirm={() => {
                setConfirmEmpty(false);
                void keep.emptyTrash();
              }}
            />
          </Show>
          <Snackbar />
          <AccountDialog open={settingsOpen()} section="import" onClose={() => setSettingsOpen(false)} />
        </div>
      </KeepContext.Provider>
    </KeepRootContext.Provider>
  );
}
