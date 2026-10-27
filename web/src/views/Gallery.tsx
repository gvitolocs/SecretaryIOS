import { A } from "@solidjs/router";
import { createEffect, createMemo, createResource, createSignal, on, onCleanup, Show } from "solid-js";
import { batch, listMonths, listSources, type GalleryItem, type GalleryKind, type GalleryQuery, type SourceKind } from "../api/gallery";
import { AlbumPicker } from "../components/gallery/AlbumPicker";
import { DeleteDialog, type DeleteResult } from "../components/gallery/DeleteDialog";
import { GalleryViewer } from "../components/gallery/GalleryViewer";
import { createGalleryFeed, SEEK_PAGE } from "../components/gallery/GridFeed";
import { GridView, type GridCommand, type TileClick } from "../components/gallery/GridView";
import { SelectionBar } from "../components/gallery/SelectionBar";
import { SourceBar } from "../components/gallery/SourceBar";
import { BackIcon, SearchIcon, TrashIcon } from "../components/Icons";
import { t } from "../i18n";
import { rangeIds, sectionKey, sourceIdOf, toggleId, type GroupBy } from "../lib/galleryLayout";
import { supportsJxl } from "../lib/jxl";
import { desktopDevice } from "../lib/desktopViewer";
import { readString, writeString } from "../lib/storage";
import "../styles/gallery.css";

// The Gallery (/gallery, and /media for the Mac shell's rail): one virtual grid of the phones', the Mac's, the old
// backups', the chat's and Lucina's pictures, switched by source, filtered, searched, selected and changed in batches.
// /gallery/trash is the same grid over the Trash. Deleting here never deletes the original on a device.

const ORIGIN_KEY = "secretary.gallery.origin";
const GROUP_KEY = "secretary.gallery.group";
const DOWNLOAD_GAP_MS = 350;

type KindFilter = GalleryKind | "all";

function storedGroup(): GroupBy {
  return readString("local", GROUP_KEY) === "month" ? "month" : "day";
}

/** The source the Gallery opens on: this Mac's own pictures in the Mac app, else everything. */
function initialOrigin(): string {
  const chosen = readString("session", ORIGIN_KEY);
  if (chosen) return chosen;
  const mac = desktopDevice();
  return mac ? `device:${mac.id}` : "all";
}

export default function GalleryView(props: { trash?: boolean }) {
  const trash = () => !!props.trash;
  const [origin, setOrigin] = createSignal(initialOrigin());
  const [kind, setKind] = createSignal<KindFilter>("all");
  const [favorite, setFavorite] = createSignal(false);
  const [from, setFrom] = createSignal("");
  const [to, setTo] = createSignal("");
  const [datesOpen, setDatesOpen] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [q, setQ] = createSignal("");
  const [group, setGroup] = createSignal<GroupBy>(storedGroup());

  const [sources, { refetch: refetchSources }] = createResource(() => !trash() || undefined, () => listSources());
  const [jxl] = createResource(supportsJxl);

  // A device that isn't in the library (this Mac has not uploaded yet, or the choice is stale) falls back to All.
  createEffect(() => {
    const data = sources();
    if (!data || trash()) return;
    const o = origin();
    if (o !== "all" && !data.sources.some((s) => s.id === o)) setOrigin("all");
  });

  createEffect(
    on(draft, (value) => {
      const timer = setTimeout(() => setQ(value.trim()), 300);
      onCleanup(() => clearTimeout(timer));
    }, { defer: true }),
  );

  const query = createMemo<GalleryQuery>(() => ({
    origin: trash() ? "all" : origin(),
    kind: kind(),
    from: from() || undefined,
    to: to() || undefined,
    q: q() || undefined,
    favorite: favorite() || undefined,
    trashed: trash() || undefined,
  }));
  const feed = createGalleryFeed(query);
  const items = feed.items;
  const filtered = () => kind() !== "all" || favorite() || !!from() || !!to() || !!q();

  const [months, { refetch: refetchMonths }] = createResource(
    () => ({ origin: trash() ? "all" : origin(), kind: kind(), favorite: favorite(), trashed: trash() }),
    (key) => listMonths(key).catch(() => []),
  );

  const kindBySource = createMemo(() => new Map<string, SourceKind>((sources()?.sources ?? []).map((s) => [s.id, s.kind])));
  const sourceKind = (item: GalleryItem): SourceKind => {
    const id = sourceIdOf(item);
    return kindBySource().get(id) ?? (id === "chat" ? "chat" : id === "lucina" ? "lucina" : "other");
  };

  // ---- status line --------------------------------------------------------------------------------------------------

  const [status, setStatus] = createSignal<{ text: string; error: boolean } | null>(null);
  let statusTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(statusTimer));
  function say(text: string, error = false) {
    clearTimeout(statusTimer);
    setStatus({ text, error });
    statusTimer = setTimeout(() => setStatus(null), error ? 8000 : 4000);
  }
  const refreshCounts = () => {
    void refetchSources();
    void refetchMonths();
  };

  // ---- selection ----------------------------------------------------------------------------------------------------

  const [selected, setSelected] = createSignal<ReadonlySet<string>>(new Set());
  const [busy, setBusy] = createSignal(false);
  let anchor: number | null = null;
  let bandBase: ReadonlySet<string> = new Set();
  const ids = () => items().map((i) => i.id);
  const selectedItems = createMemo(() => {
    const sel = selected();
    return sel.size ? items().filter((i) => sel.has(i.id)) : [];
  });
  const clearSelection = () => {
    anchor = null;
    setSelected(new Set<string>());
  };
  // A new query shows other pictures: nothing stays selected.
  createEffect(on(() => JSON.stringify(query()), clearSelection, { defer: true }));

  const toggleAt = (index: number) => {
    const item = items()[index];
    if (!item) return;
    anchor = index;
    setSelected((s) => toggleId(s, item.id));
  };

  function onTile(click: TileClick) {
    if (click.shift && anchor !== null) {
      const next = new Set(selected());
      for (const id of rangeIds(ids(), anchor, click.index)) next.add(id);
      setSelected(next);
    } else if (click.shift || click.toggle || selected().size > 0) toggleAt(click.index);
    else setViewer(click.index);
  }

  function onSection(start: number, count: number, on: boolean) {
    const next = new Set(selected());
    for (const item of items().slice(start, start + count)) {
      if (on) next.add(item.id);
      else next.delete(item.id);
    }
    setSelected(next);
  }

  function onBand(phase: "start" | "move" | "end", indexes: number[], additive: boolean) {
    if (phase === "start") bandBase = additive ? new Set(selected()) : new Set();
    else if (phase === "move") {
      const all = items();
      const next = new Set(bandBase);
      for (const i of indexes) {
        const item = all[i];
        if (item) next.add(item.id);
      }
      setSelected(next);
    }
  }

  function onCommand(command: GridCommand) {
    if (command === "clear") clearSelection();
    else if (command === "all") setSelected(new Set(ids()));
    else if (command === "delete" && selectedItems().length) setDeleting(selectedItems());
    else if (command === "favorite") void favoriteSelected();
  }

  // ---- batch actions ------------------------------------------------------------------------------------------------

  const [deleting, setDeleting] = createSignal<GalleryItem[] | null>(null);
  const [albumFor, setAlbumFor] = createSignal<string[] | null>(null);

  async function run(op: "favorite" | "unfavorite" | "restore", items: GalleryItem[]): Promise<string[]> {
    setBusy(true);
    try {
      const result = await batch(op, items.map((i) => i.id));
      if (result.failed.length) say(t("gallery.partialFail", { count: result.failed.length }), true);
      return result.done;
    } catch {
      say(t("gallery.actionFailed"), true);
      return [];
    } finally {
      setBusy(false);
    }
  }

  async function favoriteSelected() {
    const chosen = selectedItems();
    if (!chosen.length || busy()) return;
    const make = !chosen.every((i) => i.favorite);
    const done = await run(make ? "favorite" : "unfavorite", chosen);
    if (!done.length) return;
    if (!make && favorite()) feed.remove(done);
    else feed.update(done, (i) => ({ ...i, favorite: make }));
    say(t(make ? "gallery.favorited" : "gallery.unfavorited", { count: done.length }));
    refreshCounts();
  }

  async function restoreSelected() {
    const chosen = selectedItems();
    if (!chosen.length || busy()) return;
    const done = await run("restore", chosen);
    if (!done.length) return;
    feed.remove(done);
    setSelected((s) => new Set([...s].filter((id) => !done.includes(id))));
    say(t("gallery.restored", { count: done.length }));
    refreshCounts();
  }

  function downloadSelected() {
    const urls = selectedItems()
      .map((i) => i.urls.download ?? i.urls.original)
      .filter((u): u is string => !!u);
    urls.forEach((url, n) => {
      setTimeout(() => {
        const a = document.createElement("a");
        a.href = url;
        a.download = "";
        a.rel = "noopener";
        document.body.append(a);
        a.click();
        a.remove();
      }, n * DOWNLOAD_GAP_MS);
    });
  }

  function deleted(result: DeleteResult) {
    const gone = [...result.trashed, ...result.purged];
    setDeleting(null);
    if (gone.length) {
      feed.remove(gone);
      setSelected((s) => new Set([...s].filter((id) => !gone.includes(id))));
      say(t(result.purged.length ? "gallery.deletedForever" : "gallery.movedToTrash", { count: gone.length }));
      refreshCounts();
    }
    if (result.failed.length) say(t("gallery.partialFail", { count: result.failed.length }), true);
  }

  // ---- the viewer ---------------------------------------------------------------------------------------------------

  const [viewer, setViewer] = createSignal<number | null>(null);
  createEffect(() => {
    const at = viewer();
    if (at !== null && at >= items().length - 20 && feed.hasMore() && !feed.loading() && !feed.failed()) void feed.loadMore();
  });
  function removedInViewer(id: string) {
    const left = items().length - 1;
    const at = viewer();
    feed.remove([id]);
    setSelected((s) => (s.has(id) ? new Set([...s].filter((x) => x !== id)) : s));
    if (at === null) return;
    if (left <= 0) setViewer(null);
    else setViewer(Math.min(at, left - 1));
    refreshCounts();
  }

  // ---- loading more and seeking ---------------------------------------------------------------------------------------

  async function loadUntil(month: string) {
    while (feed.hasMore() && !feed.failed()) {
      const last = items()[items().length - 1];
      if (last && sectionKey(last, "month") <= month) break;
      await feed.loadMore(SEEK_PAGE);
    }
  }

  // ---- the toolbar ----------------------------------------------------------------------------------------------------

  const chooseOrigin = (id: string) => {
    setOrigin(id);
    writeString("session", ORIGIN_KEY, id);
  };
  const chooseGroup = (g: GroupBy) => {
    setGroup(g);
    writeString("local", GROUP_KEY, g);
  };
  const clearFilters = () => {
    setKind("all");
    setFavorite(false);
    setFrom("");
    setTo("");
    setDraft("");
    setQ("");
  };

  const emptyText = () => (trash() ? t("gallery.emptyTrash") : filtered() ? t("gallery.emptyFiltered") : t("gallery.empty"));
  const settled = () => !feed.loading() && feed.items().length === 0 && !feed.hasMore();
  const seg = (value: KindFilter, label: string) => (
    <button type="button" class="g-seg__btn" aria-pressed={kind() === value} onClick={() => setKind(value)}>
      {label}
    </button>
  );

  return (
    <section class="gallery" classList={{ "gallery--trash": trash() }} aria-labelledby="gallery-title">
      <div class="g-top">
        <Show
          when={selected().size > 0}
          fallback={
            <header class="g-bar">
              <Show when={trash()}>
                <A class="g-back" href="/gallery" aria-label={t("gallery.backToGallery")}>
                  <BackIcon />
                </A>
              </Show>
              <h1 class="g-title" id="gallery-title">
                {trash() ? t("gallery.trashTitle") : t("gallery.title")}
              </h1>
              <label class="g-search">
                <SearchIcon />
                <input
                  type="search"
                  value={draft()}
                  placeholder={t("gallery.searchPlaceholder")}
                  aria-label={t("gallery.search")}
                  onInput={(e) => setDraft(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") setQ(draft().trim());
                  }}
                />
              </label>
              <Show when={!trash()}>
                <A class="g-iconbtn" href="/gallery/trash" aria-label={t("gallery.trashLink")} title={t("gallery.trashLink")}>
                  <TrashIcon />
                </A>
              </Show>
            </header>
          }
        >
          <SelectionBar
            count={selected().size}
            all={selected().size >= items().length}
            allFavorite={selectedItems().length > 0 && selectedItems().every((i) => i.favorite)}
            trash={trash()}
            busy={busy()}
            onSelectAll={() => onCommand("all")}
            onClear={clearSelection}
            onFavorite={() => void favoriteSelected()}
            onAlbum={() => setAlbumFor(selectedItems().map((i) => i.id))}
            onDownload={downloadSelected}
            onDelete={() => onCommand("delete")}
            onRestore={() => void restoreSelected()}
          />
        </Show>
        <Show when={!trash() && (sources()?.sources.length ?? 0) > 0}>
          <SourceBar sources={sources()!.sources} all={sources()!.all} value={origin()} onChange={chooseOrigin} />
        </Show>
        <div class="g-filters" role="group" aria-label={t("gallery.filters")}>
          <div class="g-seg" role="group" aria-label={t("gallery.typeLabel")}>
            {seg("all", t("gallery.typeAll"))}
            {seg("photo", t("gallery.typePhotos"))}
            {seg("video", t("gallery.typeVideos"))}
          </div>
          <Show when={!trash()}>
            <button type="button" class="g-chip" aria-pressed={favorite()} onClick={() => setFavorite((v) => !v)}>
              {t("gallery.favorites")}
            </button>
          </Show>
          <button
            type="button"
            class="g-chip"
            aria-expanded={datesOpen()}
            classList={{ "g-chip--on": !!from() || !!to() }}
            onClick={() => setDatesOpen((v) => !v)}
          >
            {t("gallery.dates")}
          </button>
          <Show when={datesOpen() || from() || to()}>
            <span class="g-dates">
              <label>
                <span>{t("gallery.dateFrom")}</span>
                <input type="date" value={from()} max={to() || undefined} onInput={(e) => setFrom(e.currentTarget.value)} />
              </label>
              <label>
                <span>{t("gallery.dateTo")}</span>
                <input type="date" value={to()} min={from() || undefined} onInput={(e) => setTo(e.currentTarget.value)} />
              </label>
              <Show when={from() || to()}>
                <button
                  type="button"
                  class="g-chip"
                  onClick={() => {
                    setFrom("");
                    setTo("");
                  }}
                >
                  {t("gallery.clearDates")}
                </button>
              </Show>
            </span>
          </Show>
          <Show when={filtered()}>
            <button type="button" class="g-chip g-chip--quiet" onClick={clearFilters}>
              {t("gallery.clearFilters")}
            </button>
          </Show>
          <div class="g-seg g-seg--end" role="group" aria-label={t("gallery.groupLabel")}>
            <button type="button" class="g-seg__btn" aria-pressed={group() === "day"} onClick={() => chooseGroup("day")}>
              {t("gallery.groupDay")}
            </button>
            <button type="button" class="g-seg__btn" aria-pressed={group() === "month"} onClick={() => chooseGroup("month")}>
              {t("gallery.groupMonth")}
            </button>
          </div>
        </div>
        <Show when={trash()}>
          <p class="g-note">{t("gallery.trashNote")}</p>
        </Show>
      </div>

      <div class="g-main">
        <GridView
          items={items()}
          epoch={feed.epoch()}
          resetKey={JSON.stringify(query())}
          groupBy={group()}
          selected={selected()}
          jxl={jxl() ?? false}
          showSource={!trash() && origin() === "all"}
          sourceKind={sourceKind}
          trash={trash()}
          loading={feed.loading()}
          hasMore={feed.hasMore()}
          failed={feed.failed()}
          months={months() ?? []}
          scrubber={!q() && !from() && !to()}
          onNeedMore={() => void feed.loadMore()}
          onRetry={() => void feed.loadMore()}
          onSeek={loadUntil}
          onTile={onTile}
          onToggle={toggleAt}
          onLongPress={(i) => {
            if (!selected().has(items()[i]?.id ?? "")) toggleAt(i);
          }}
          onSection={onSection}
          onBand={onBand}
          onCommand={onCommand}
        />
        <Show when={items().length === 0}>
          <div class="g-state">
            <Show when={feed.failed()} fallback={
              <Show when={settled()} fallback={<span class="g-spinner" role="status" aria-label={t("gallery.loading")} />}>
                <p>{emptyText()}</p>
                <Show when={filtered()}>
                  <button type="button" class="g-chip" onClick={clearFilters}>
                    {t("gallery.clearFilters")}
                  </button>
                </Show>
              </Show>
            }>
              <p role="alert">{t("gallery.loadError")}</p>
              <button type="button" class="g-chip" onClick={() => feed.reload()}>
                {t("gallery.retry")}
              </button>
            </Show>
          </div>
        </Show>
        <Show when={status()}>
          {(s) => (
            <div class="g-status" classList={{ "g-status--error": s().error }} role={s().error ? "alert" : "status"}>
              <span>{s().text}</span>
              <button type="button" onClick={() => setStatus(null)} aria-label={t("gallery.dismiss")}>
                ×
              </button>
            </div>
          )}
        </Show>
      </div>

      <Show when={viewer() !== null && items()[viewer()!]}>
        <GalleryViewer
          items={items()}
          index={viewer()!}
          onIndex={setViewer}
          onClose={() => setViewer(null)}
          onChanged={feed.replace}
          onRemoved={removedInViewer}
          trash={trash()}
        />
      </Show>
      <Show when={deleting()}>
        {(list) => <DeleteDialog items={list()} purge={trash()} onDone={deleted} onCancel={() => setDeleting(null)} />}
      </Show>
      <Show when={albumFor()}>
        {(list) => (
          <AlbumPicker
            itemIds={list()}
            onDone={() => {
              say(t("gallery.addedToAlbum", { count: list().length }));
              setAlbumFor(null);
              void refetchSources();
            }}
            onCancel={() => setAlbumFor(null)}
          />
        )}
      </Show>
    </section>
  );
}
