import { A, useNavigate, useSearchParams } from "@solidjs/router";
import { createEffect, createMemo, createResource, createSignal, on, onCleanup, Show } from "solid-js";
import {
  batch,
  getAlbum,
  getEmbeddingStatus,
  getPerson,
  listGallery,
  listMonths,
  listPersonItems,
  listSimilar,
  listSources,
  searchSmart,
  type EmbeddingStatus,
  type GalleryItem,
  type GalleryKind,
  type GalleryQuery,
  type SourceKind,
} from "../api/gallery";
import { AlbumPicker } from "../components/gallery/AlbumPicker";
import { AlbumTools } from "../components/gallery/albums/AlbumTools";
import { DeleteDialog, type DeleteResult } from "../components/gallery/DeleteDialog";
import { GalleryViewer } from "../components/gallery/GalleryViewer";
import { createGalleryFeed, SEEK_PAGE } from "../components/gallery/GridFeed";
import { GridView, type GridCommand, type TileClick } from "../components/gallery/GridView";
import { SelectionBar } from "../components/gallery/SelectionBar";
import { ArchiveBoxIcon, PlacesIcon } from "../components/gallery/browse/BrowseIcons";
import { PeopleIcon } from "../components/gallery/faces/FaceIcons";
import { PersonHeader } from "../components/gallery/people/PersonHeader";
import { MemoriesStrip } from "../components/gallery/browse/MemoriesStrip";
import { SourceBar } from "../components/gallery/SourceBar";
import { SmartHeader } from "../components/gallery/smart/SmartHeader";
import { AlbumIcon } from "../components/gallery/ViewerIcons";
import { LabIcon } from "../components/gallery/lab/LabIcons";
import { ShareSheet } from "../components/gallery/share/ShareSheet";
import { BackIcon, LinkIcon, SearchIcon, TrashIcon } from "../components/Icons";
import { t } from "../i18n";
import { ALBUMS_PATH } from "../lib/galleryAlbums";
import { PEOPLE_PATH, personName, personPath } from "../lib/galleryPeople";
import { rangeIds, sectionKey, sourceIdOf, toggleId, type GroupBy } from "../lib/galleryLayout";
import { lookedAt, smartPager, smartReady, usesSmart, withSearch, type SearchMode } from "../lib/gallerySmart";
import { supportsJxl } from "../lib/jxl";
import { desktopDevice } from "../lib/desktopViewer";
import { readString, writeString } from "../lib/storage";
import "../styles/gallery.css";

// The Gallery (/gallery, and /media for the Mac shell's rail): one virtual grid of the phones', the Mac's, the old
// backups', the chat's and Lucina's pictures, switched by source, filtered, searched, selected and changed in batches.
// /gallery/trash is the same grid over the Trash. Deleting here never deletes the original on a device.

const ORIGIN_KEY = "secretary.gallery.origin";
const GROUP_KEY = "secretary.gallery.group";
const MODE_KEY = "secretary.gallery.search";
const DOWNLOAD_GAP_MS = 350;

type KindFilter = GalleryKind | "all";

function storedGroup(): GroupBy {
  return readString("local", GROUP_KEY) === "month" ? "month" : "day";
}

function storedMode(): SearchMode {
  return readString("local", MODE_KEY) === "words" ? "words" : "smart";
}

/** The source the Gallery opens on: this Mac's own pictures in the Mac app, else everything. */
function initialOrigin(): string {
  const chosen = readString("session", ORIGIN_KEY);
  if (chosen) return chosen;
  const mac = desktopDevice();
  return mac ? `device:${mac.id}` : "all";
}

export default function GalleryView(props: { trash?: boolean; archive?: boolean; album?: string; person?: string }) {
  const trash = () => !!props.trash;
  const archive = () => !!props.archive;
  const albumId = () => props.album;
  const personId = () => props.person;
  // The shelves, an album and a person list everything of theirs, whatever source the Gallery was last switched to.
  const shelf = () => trash() || archive() || !!albumId() || !!personId();
  // `?bbox=w,s,e,n` shows the pictures of one Places cluster.
  const [params, setParams] = useSearchParams<{ bbox?: string }>();
  const bbox = () => (typeof params.bbox === "string" && params.bbox ? params.bbox : undefined);
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
  const navigate = useNavigate();
  const [album, { mutate: setAlbum }] = createResource(albumId, (id) => getAlbum(id).catch(() => null));
  const albumGone = () => !!albumId() && !album.loading && !album();
  // A person's page: the same grid over the photos with that person's face (the people API pages them).
  const [person, { mutate: setPerson, refetch: refetchPerson }] = createResource(personId, (id) => getPerson(id).catch(() => null));
  const personGone = () => !!personId() && !person.loading && !person();

  // ---- smart search: the box asks by meaning when the server can, else by words ---------------------------------------
  const smartAllowed = () => !trash() && !archive();
  const [fetchedStatus] = createResource(() => smartAllowed() || undefined, () => getEmbeddingStatus());
  const [latestStatus, setLatestStatus] = createSignal<EmbeddingStatus | null>(null);
  const smartStatus = () => latestStatus() ?? fetchedStatus() ?? null;
  // The server answered 503 (smart search off): words from now on, without a word about it.
  const [refused, setRefused] = createSignal(false);
  const [mode, setMode] = createSignal<SearchMode>(storedMode());
  const [similar, setSimilar] = createSignal<{ id: string; title: string } | null>(null);

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

  const smartOn = createMemo(() => smartAllowed() && usesSmart({ q: q(), mode: mode(), status: smartStatus(), refused: refused() }));
  const similarOn = createMemo(() => smartAllowed() && !refused() && similar() !== null);
  // Results by meaning are ordered by relevance, not by date: one flat list with no day headers.
  const flat = () => smartOn() || similarOn();
  createEffect(on(q, () => setSimilar(null), { defer: true }));

  const query = createMemo<GalleryQuery>(() =>
    withSearch(
      {
        origin: shelf() ? "all" : origin(),
        kind: kind(),
        from: from() || undefined,
        to: to() || undefined,
        q: q() || undefined,
        favorite: favorite() || undefined,
        trashed: trash() || undefined,
        archived: archive() || undefined,
        bbox: bbox(),
        album: albumId(),
        person: personId(),
      },
      { smart: smartOn(), similarTo: similarOn() ? similar()!.id : null },
    ),
  );
  const pager = smartPager((request) => (request.similarTo ? listSimilar(request.similarTo, request) : searchSmart(request)), {
    onUnavailable: () => {
      setRefused(true);
      setSimilar(null);
    },
    onStatus: setLatestStatus,
  }, (request) => (request.person ? listPersonItems(request.person, request) : listGallery(request)));
  const feed = createGalleryFeed(query, pager);
  const items = feed.items;
  const filtered = () => kind() !== "all" || favorite() || !!from() || !!to() || !!q() || !!bbox() || similarOn();

  const [months, { refetch: refetchMonths }] = createResource(
    // The scrubber counts the whole library, so an album doesn't ask for it.
    () => (albumId() || personId() ? false : { origin: shelf() ? "all" : origin(), kind: kind(), favorite: favorite(), trashed: trash(), archived: archive() }),
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
  const [sharing, setSharing] = createSignal<GalleryItem[] | null>(null);

  async function run(op: "favorite" | "unfavorite" | "restore" | "archive" | "unarchive" | "album_remove", items: GalleryItem[], inAlbum?: string): Promise<string[]> {
    setBusy(true);
    try {
      const result = await batch(op, items.map((i) => i.id), inAlbum ? { album: inAlbum } : {});
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

  // Archive / Unarchive: the pictures leave this list (the Gallery or the Archive) for the other.
  async function archiveSelected() {
    const chosen = selectedItems();
    if (!chosen.length || busy()) return;
    const done = await run(archive() ? "unarchive" : "archive", chosen);
    if (!done.length) return;
    feed.remove(done);
    setSelected((s) => new Set([...s].filter((id) => !done.includes(id))));
    say(t(archive() ? "browse.unarchived" : "browse.archived", { count: done.length }));
    refreshCounts();
  }

  // Inside an album: the chosen photos leave the album, never the Gallery.
  async function removeFromAlbum() {
    const chosen = selectedItems();
    const id = albumId();
    if (!chosen.length || !id || busy()) return;
    const done = await run("album_remove", chosen, id);
    if (!done.length) return;
    feed.remove(done);
    setSelected((s) => new Set([...s].filter((x) => !done.includes(x))));
    setAlbum((a) => (a ? { ...a, count: Math.max(0, a.count - done.length) } : a));
    say(t("galleryAlbums.removed", { count: done.length }));
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
  // "Similar photos": the grid shows the pictures closest to this one, and the viewer closes over it.
  function showSimilar(item: GalleryItem) {
    setViewer(null);
    setSimilar({ id: item.id, title: item.title || item.origin.filename || "" });
  }
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
  const chooseMode = (m: SearchMode) => {
    setMode(m);
    writeString("local", MODE_KEY, m);
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
    setSimilar(null);
    if (bbox()) setParams({ bbox: undefined });
  };

  const emptyText = () =>
    trash()
      ? t("gallery.emptyTrash")
      : archive() && !filtered()
        ? t("browse.emptyArchive")
        : similarOn()
          ? t("gallerySmart.emptySimilar")
          : smartOn()
            ? t("gallerySmart.emptySearch")
            : filtered()
              ? t("gallery.emptyFiltered")
              : albumGone()
                ? t("galleryAlbums.albumMissing")
                : albumId()
                  ? t("galleryAlbums.emptyAlbum")
                  : personGone()
                    ? t("galleryPeople.personMissing")
                    : personId()
                      ? t("galleryPeople.emptyPerson")
                      : t("gallery.empty");
  const settled = () => !feed.loading() && feed.items().length === 0 && !feed.hasMore();
  const seg = (value: KindFilter, label: string) => (
    <button type="button" class="g-seg__btn" aria-pressed={kind() === value} onClick={() => setKind(value)}>
      {label}
    </button>
  );

  return (
    <section class="gallery" classList={{ "gallery--trash": trash(), "gallery--archive": archive(), "gallery--album": !!albumId(), "gallery--person": !!personId() }} aria-labelledby="gallery-title">
      <div class="g-top">
        <Show
          when={selected().size > 0}
          fallback={
            <header class="g-bar">
              <Show when={shelf()}>
                <A class="g-back" href={albumId() ? ALBUMS_PATH : personId() ? PEOPLE_PATH : "/gallery"} aria-label={albumId() ? t("galleryAlbums.title") : personId() ? t("galleryPeople.backToPeople") : t("gallery.backToGallery")}>
                  <BackIcon />
                </A>
              </Show>
              <h1 class="g-title" id="gallery-title">
                {trash() ? t("gallery.trashTitle") : archive() ? t("browse.archiveTitle") : albumId() ? album()?.title || t("galleryAlbums.title") : personId() ? (person() ? personName(person()!, t("galleryPeople.unnamed")) : t("galleryPeople.title")) : t("gallery.title")}
              </h1>
              <label class="g-search">
                <SearchIcon />
                <input
                  type="search"
                  value={draft()}
                  placeholder={person() ? (person()!.name ? t("galleryPeople.searchWithin", { name: person()!.name! }) : t("galleryPeople.searchWithinUnnamed")) : t("gallery.searchPlaceholder")}
                  aria-label={t("gallery.search")}
                  onInput={(e) => setDraft(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") setQ(draft().trim());
                  }}
                />
              </label>
              <Show when={album()}>{(a) => <AlbumTools album={a()} onRenamed={setAlbum} onDeleted={() => navigate(ALBUMS_PATH)} />}</Show>
              <Show when={!shelf()}>
                <A class="g-iconbtn" href="/gallery/albums" aria-label={t("galleryAlbums.link")} title={t("galleryAlbums.link")}>
                  <AlbumIcon />
                </A>
                <A class="g-iconbtn" href={PEOPLE_PATH} aria-label={t("galleryPeople.link")} title={t("galleryPeople.link")}>
                  <PeopleIcon />
                </A>
                <A class="g-iconbtn" href="/gallery/places" aria-label={t("browse.placesLink")} title={t("browse.placesLink")}>
                  <PlacesIcon />
                </A>
                <A class="g-iconbtn" href="/gallery/archive" aria-label={t("browse.archiveLink")} title={t("browse.archiveLink")}>
                  <ArchiveBoxIcon />
                </A>
                <A class="g-iconbtn" href="/gallery/lab" aria-label={t("lab.labLink")} title={t("lab.labLink")}>
                  <LabIcon />
                </A>
                <A class="g-iconbtn" href="/gallery/shared" aria-label={t("galleryShare.manage")} title={t("galleryShare.manage")}>
                  <LinkIcon />
                </A>
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
            onShare={() => setSharing(selectedItems())}
            onDownload={downloadSelected}
            onDelete={() => onCommand("delete")}
            onRestore={() => void restoreSelected()}
            archive={archive()}
            onArchive={() => void archiveSelected()}
            onRemoveFromAlbum={albumId() ? () => void removeFromAlbum() : undefined}
          />
        </Show>
        <Show when={!shelf() && (sources()?.sources.length ?? 0) > 0}>
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
          <Show when={!flat()}>
            <div class="g-seg g-seg--end" role="group" aria-label={t("gallery.groupLabel")}>
              <button type="button" class="g-seg__btn" aria-pressed={group() === "day"} onClick={() => chooseGroup("day")}>
                {t("gallery.groupDay")}
              </button>
              <button type="button" class="g-seg__btn" aria-pressed={group() === "month"} onClick={() => chooseGroup("month")}>
                {t("gallery.groupMonth")}
              </button>
            </div>
          </Show>
        </div>
        <Show when={smartAllowed() && !refused() && (similarOn() || (!!q() && smartReady(smartStatus())))}>
          <SmartHeader
            query={similarOn() ? null : q()}
            similarTo={similarOn() ? similar()!.title : null}
            mode={mode()}
            smart={smartOn()}
            looked={smartOn() || similarOn() ? lookedAt(smartStatus()) : null}
            onMode={chooseMode}
            onClearSimilar={() => setSimilar(null)}
          />
        </Show>
        <Show when={person()}>{(p) => <PersonHeader person={p()} onChanged={setPerson} />}</Show>
        <Show when={trash()}>
          <p class="g-note">{t("gallery.trashNote")}</p>
        </Show>
        <Show when={archive()}>
          <p class="g-note">{t("browse.archiveNote")}</p>
        </Show>
        <Show when={bbox()}>
          <p class="g-note g-place">
            <PlacesIcon />
            <span>{t("browse.placeFilter")}</span>
            <button type="button" class="g-chip" onClick={() => setParams({ bbox: undefined })}>
              {t("browse.placeFilterClear")}
            </button>
          </p>
        </Show>
        <Show when={!shelf() && !filtered()}>
          <MemoriesStrip origin={origin()} onMutated={() => feed.reload()} />
        </Show>
      </div>

      <div class="g-main">
        <GridView
          items={items()}
          epoch={feed.epoch()}
          resetKey={JSON.stringify(query())}
          groupBy={flat() ? "none" : group()}
          selected={selected()}
          jxl={jxl() ?? false}
          showSource={!shelf() && origin() === "all"}
          sourceKind={sourceKind}
          trash={trash()}
          loading={feed.loading()}
          hasMore={feed.hasMore()}
          failed={feed.failed()}
          months={months() ?? []}
          scrubber={!q() && !from() && !to() && !bbox() && !albumId() && !personId() && !similarOn()}
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
          archive={archive()}
          onSimilar={smartAllowed() && !refused() && smartReady(smartStatus()) ? showSimilar : undefined}
          onPerson={(id) => {
            setViewer(null);
            navigate(personPath(id));
          }}
          onPeopleChanged={personId() ? () => void refetchPerson() : undefined}
        />
      </Show>
      <Show when={deleting()}>
        {(list) => <DeleteDialog items={list()} purge={trash()} onDone={deleted} onCancel={() => setDeleting(null)} />}
      </Show>
      <Show when={sharing()}>
        {(list) => <ShareSheet target={{ kind: "items", items: list() }} onClose={() => setSharing(null)} />}
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
