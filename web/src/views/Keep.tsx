import { useLocation, useNavigate } from "@solidjs/router";
import { createEffect, createMemo, createSignal, on, onCleanup, onMount, Show, type JSX } from "solid-js";
import type { KeepNote } from "../api/keep";
import { AccountDialog } from "../components/AccountDialog";
import { KeepContext, KeepRootContext, type KeepCtx } from "../components/keep/ctx";
import { Composer } from "../components/keep/Composer";
import { ConfirmDialog, LabelsDialog } from "../components/keep/Dialogs";
import { ArchiveIcon, BellIcon, BulbOutlineIcon, LabelIcon, TrashIcon } from "../components/keep/icons";
import { Masonry } from "../components/keep/Masonry";
import { Nav } from "../components/keep/Nav";
import { NoteCard } from "../components/keep/NoteCard";
import { NoteDialog } from "../components/keep/NoteDialog";
import { SearchPage } from "../components/keep/SearchPage";
import { SelectionBar } from "../components/keep/SelectionBar";
import { Snackbar } from "../components/keep/Snackbar";
import { TopBar } from "../components/keep/TopBar";
import { moveItem } from "../components/keep/ChecklistEditor";
import { t } from "../i18n";
import { positionBetween, searchActive, searchNotes, splitPinned, viewNotes, emptySearch, type KeepView } from "../lib/keepFilter";
import { keepHref, noteHref, parseKeepPath } from "../lib/keepRoute";
import { keep, startAutoSync } from "../state/keep";
import { clearSelection, drawerOpen, listMode, navCollapsed, search, searchOpen, selection, setDrawerOpen, setListMode, setNavCollapsed, setSearch, setSearchOpen } from "../state/keepUi";
import { effectiveTheme } from "../state/appearance";
import "../styles/keep.css";

// Keep: Google-Keep-style notes at /keep (the owner's own layout, moved here from keep.google.com).
// Routes: /keep, /keep/reminders, /keep/archive, /keep/trash, /keep/label/<name>, /keep/note/<id> (a note's dialog).
// The cache, sync and optimistic writes live in state/keep.ts; everything on this page reads that store.

function EmptyState(props: { icon: () => JSX.Element; text: string }) {
  return (
    <div class="kp-empty" role="status">
      <span class="kp-empty-icon">{props.icon()}</span>
      <p>{props.text}</p>
    </div>
  );
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
  const theme = effectiveTheme;
  const searching = createMemo(() => searchActive(search()));

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
    if (searching()) return searchNotes(all, search(), keep.labelName);
    return viewNotes(all, view());
  });
  const sections = createMemo(() => {
    const list = shown();
    if (view().kind === "trash" && !searching()) return { pinned: [] as KeepNote[], others: list };
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

  return (
    <KeepRootContext.Provider value={rootEl}>
      <KeepContext.Provider value={ctx}>
        <div
          class="kp"
          ref={setRootEl}
          data-theme={theme()}
          data-nav={navCollapsed() ? "collapsed" : "open"}
          data-drawer={drawerOpen() ? "open" : "closed"}
          data-mode={listMode() ? "list" : "grid"}
          data-selecting={selection().size > 0 ? "yes" : undefined}
          data-searching={searchOpen() ? "yes" : undefined}
        >
          <Show when={selection().size > 0} fallback={
            <TopBar
              search={search()}
              searchOpen={searchOpen()}
              theme={theme()}
              labelName={keep.labelName}
              listMode={listMode()}
              syncing={keep.status() === "syncing"}
              onMenu={() => {
                if (typeof matchMedia === "function" && matchMedia("(max-width: 600px)").matches) setDrawerOpen(!drawerOpen());
                else setNavCollapsed(!navCollapsed());
              }}
              onHome={() => go("/keep")}
              onSearchFocus={() => setSearchOpen(true)}
              onSearch={(p) => setSearch({ ...search(), ...p })}
              onSearchExit={() => {
                setSearch(emptySearch());
                setSearchOpen(false);
              }}
              onRefresh={() => void keep.sync()}
              onToggleList={() => setListMode(!listMode())}
              onSettings={() => setSettingsOpen(true)}
            />
          }>
            <SelectionBar notes={selectedNotes()} />
          </Show>
          <div class="kp-body">
            <Show when={drawerOpen()}>
              <div class="kp-drawer-scrim" onClick={() => setDrawerOpen(false)} />
            </Show>
            <Nav view={view()} labels={keep.labels()} collapsed={navCollapsed()} onGo={go} onEditLabels={() => { setDrawerOpen(false); setLabelsOpen(true); }} />
            <main class="kp-main" id="keep-main" aria-label={t("keep.title")}>
              <Show when={keep.status() === "error" && keep.error()}>
                <p class="kp-sync-error" role="status">
                  {t("keep.syncFailed")}
                </p>
              </Show>
              <Show when={showFilters()}>
                <SearchPage
                  labels={keep.labels()}
                  theme={theme()}
                  onPick={(p) => setSearch({ ...search(), ...p })}
                />
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
                <Show when={keep.loaded()} fallback={<div class="kp-loading" aria-hidden="true" />}>
                  <Show
                    when={shown().length > 0}
                    fallback={
                      <EmptyState icon={emptyFor().icon} text={emptyFor().text} />
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
            </main>
          </div>
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
