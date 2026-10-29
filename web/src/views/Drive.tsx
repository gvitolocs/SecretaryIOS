import { A, useLocation, useNavigate } from "@solidjs/router";
import {
  createEffect,
  createMemo,
  createResource,
  createSignal,
  For,
  Match,
  on,
  onCleanup,
  onMount,
  Show,
  Switch,
  type Accessor,
} from "solid-js";
import { ApiError } from "../api/client";
import { driveApi } from "../api/drive";
import type { MediaItem as PlayerItem } from "../api/player";
import {
  BackIcon,
  CloseIcon,
  CopyIcon,
  DownloadIcon,
  ExternalIcon,
  FileIcon,
  FolderIcon,
  SearchIcon,
  ShareIcon,
  SortIcon,
} from "../components/Icons";
import { locale, t } from "../i18n";
import {
  Button,
  Card,
  EmptyState,
  ErrorState,
  ListRow,
  LoadingState,
  Page,
  PageGrid,
  PageHeader,
  RowList,
  useWide,
} from "../components/page";
import {
  albumHref,
  type ArchiveListing,
  breadcrumbs,
  defaultOrder,
  DRIVE_SORTS,
  DRIVE_TYPES,
  downloadUrl,
  type DriveFilters,
  type DriveFolderEntry,
  type DriveItem,
  type DriveSort,
  type DriveType,
  fileHref,
  filterLabelKey,
  filtersSearch,
  folderHref,
  formatBytes,
  formatModified,
  imageUrl,
  itemLocation,
  itemModified,
  itemName,
  itemPreview,
  itemType,
  openParam,
  parseDriveRoute,
  parseFilters,
  previewLines,
  SHARE_FILE_LIMIT,
  type TextPreview,
  thumbnailUrl,
  tokenClass,
  typeBadge,
  typeLabelKey,
} from "../lib/drive";
import { supportsJxl } from "../lib/jxl";
import { media, playQueue } from "../state/media";
import { MiniPlayer, Player } from "./Media";
import "../styles/media.css";
import "../styles/drive.css";

// The Drive (/drive, a Plugins card): every file in the owner's Drive folders on lucina-media,
// browsed by folder, filtered by type, searched by name and content, previewed in place.
// Routes: /drive, /drive/folder/<folder>/<path…>, /drive/album/<id>, /drive/file/<id>
// (Lucina's chat links to these), with ?q=&type=&sort=&order= and ?open=<id> for a preview.

type Context = { kind: "roots" } | { kind: "folder"; folder: string; path: string } | { kind: "album"; id: string };

type Next = { offset: number } | { cursor: string } | null;

function sameContext(a: Context | null, b: Context | null): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function contextHref(c: Context): string {
  if (c.kind === "folder") return folderHref(c.folder, c.path);
  if (c.kind === "album") return albumHref(c.id);
  return "/drive";
}

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** Show a long list a chunk at a time so thousands of rows never block the page. */
function progressive(length: Accessor<number>, chunk = 400): Accessor<number> {
  const [shown, setShown] = createSignal(chunk);
  let timer: ReturnType<typeof setTimeout> | undefined;
  // A new list (another file) starts again from the first chunk.
  createEffect(on(length, () => setShown(chunk), { defer: true }));
  createEffect(() => {
    const n = length();
    const s = shown();
    clearTimeout(timer);
    if (s < n) timer = setTimeout(() => setShown(s + chunk), 30);
  });
  onCleanup(() => clearTimeout(timer));
  return shown;
}

// ---------------------------------------------------------------- icons

export function FileGlyph(props: { type: DriveType; ext?: string; thumb?: string | null }) {
  const [failed, setFailed] = createSignal(false);
  const badge = () => typeBadge(props.type, props.ext ?? "");
  return (
    <Show
      when={props.thumb && !failed()}
      fallback={
        <span class="drive-glyph" data-tone={badge().tone} data-type={props.type} aria-hidden="true">
          <FileIcon />
          <Show when={badge().badge}>
            <span class="drive-glyph__badge">{badge().badge}</span>
          </Show>
        </span>
      }
    >
      <img class="drive-thumb" src={props.thumb!} alt="" loading="lazy" onError={() => setFailed(true)} />
    </Show>
  );
}

function FolderGlyph() {
  return (
    <span class="drive-glyph drive-glyph--folder" aria-hidden="true">
      <FolderIcon />
    </span>
  );
}

// ---------------------------------------------------------------- rows

function fileMeta(item: DriveItem): string {
  return [t(`drive.${typeLabelKey(itemType(item))}`), formatBytes(item.size, locale()), formatModified(itemModified(item), locale())]
    .filter(Boolean)
    .join(" · ");
}

function FolderRow(props: { name: string; detail: string; onOpen: () => void }) {
  return (
    <li>
      <button type="button" class="drive-row" data-kind="folder" onClick={() => props.onOpen()}>
        <FolderGlyph />
        <span class="drive-row__text">
          <span class="drive-row__name">{props.name}</span>
          <span class="drive-row__meta">{props.detail}</span>
        </span>
      </button>
    </li>
  );
}

function FileRow(props: { item: DriveItem; jxl: boolean; showLocation: boolean; onOpen: () => void }) {
  const location = () => (props.showLocation ? itemLocation(props.item) : "");
  return (
    <li>
      <button type="button" class="drive-row" data-kind="file" data-drive-type={itemType(props.item)} data-id={props.item.id} onClick={() => props.onOpen()}>
        <FileGlyph type={itemType(props.item)} ext={props.item.drive?.ext} thumb={thumbnailUrl(props.item, props.jxl)} />
        <span class="drive-row__text">
          <span class="drive-row__name">{itemName(props.item)}</span>
          <span class="drive-row__meta">{fileMeta(props.item)}</span>
          <Show when={location()}>
            <span class="drive-row__where">
              <FolderIcon /> {location()}
            </span>
          </Show>
          <Show when={props.item.snippet}>
            <span class="drive-row__snippet">{props.item.snippet}</span>
          </Show>
        </span>
      </button>
    </li>
  );
}

function DeskFolderRow(props: { name: string; detail: string; onOpen: () => void }) {
  return (
    <ListRow
      leading={<FolderGlyph />}
      title={
        <button type="button" class="pg-row-name" onClick={() => props.onOpen()}>
          {props.name}
        </button>
      }
      sub={props.detail}
    />
  );
}

function DeskFileRow(props: { item: DriveItem; jxl: boolean; showLocation: boolean; onOpen: () => void }) {
  const location = () => (props.showLocation ? itemLocation(props.item) : "");
  return (
    <ListRow
      leading={<FileGlyph type={itemType(props.item)} ext={props.item.drive?.ext} thumb={thumbnailUrl(props.item, props.jxl)} />}
      title={
        <button type="button" class="pg-row-name" onClick={() => props.onOpen()}>
          {itemName(props.item)}
        </button>
      }
      sub={
        <>
          <Show when={location()}>
            <span>
              <FolderIcon /> {location()}
            </span>
          </Show>
          <Show when={props.item.snippet}>
            <span>{props.item.snippet}</span>
          </Show>
        </>
      }
      meta={fileMeta(props.item)}
    />
  );
}

function folderDetail(f: { files: number | null; bytes: number | null; modified: string | null }): string {
  return [
    f.files !== null ? t("drive.files", { count: f.files }) : "",
    formatBytes(f.bytes, locale()),
    formatModified(f.modified, locale()),
  ]
    .filter(Boolean)
    .join(" · ");
}

// ---------------------------------------------------------------- previews

function Pending() {
  return (
    <p class="drive-note" role="status">
      {t("drive.pending")}
    </p>
  );
}

function PdfView(props: { url: string }) {
  return (
    <object class="drive-pdf" data={props.url} type="application/pdf" aria-label="PDF">
      <p class="drive-note">
        {t("drive.pdfFallback")}{" "}
        <a href={props.url} target="_blank" rel="noopener">
          {t("drive.openNewTab")}
        </a>
      </p>
    </object>
  );
}

type TextState =
  | { status: "loading" }
  | { status: "pending" }
  | { status: "ready"; preview: Extract<TextPreview, { state: "ready" }> }
  | { status: "empty" }
  | { status: "error"; message: string };

const TEXT_RETRY_MS = 3000;
const TEXT_RETRIES = 60;

export function TextView(props: { url: string }) {
  const [state, setState] = createSignal<TextState>({ status: "loading" });
  const [copied, setCopied] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let token = 0;

  function load(url: string, attempt: number, mine: number) {
    driveApi
      .text(url)
      .then((p) => {
        if (mine !== token) return;
        if (p.state === "pending") {
          setState({ status: "pending" });
          if (attempt < TEXT_RETRIES) timer = setTimeout(() => load(url, attempt + 1, mine), TEXT_RETRY_MS);
          return;
        }
        setState(p.text || (p.lines?.length ?? 0) > 0 ? { status: "ready", preview: p } : { status: "empty" });
      })
      .catch((e: unknown) => {
        if (mine !== token) return;
        if (e instanceof ApiError && e.status === 404) setState({ status: "empty" });
        else setState({ status: "error", message: errorText(e) });
      });
  }

  createEffect(
    on(
      () => props.url,
      (url) => {
        clearTimeout(timer);
        const mine = ++token;
        setState({ status: "loading" });
        load(url, 0, mine);
      },
    ),
  );
  onCleanup(() => {
    token++;
    clearTimeout(timer);
  });

  const ready = () => {
    const s = state();
    return s.status === "ready" ? s.preview : null;
  };
  const lines = createMemo(() => {
    const p = ready();
    return p ? previewLines(p) : [];
  });
  const shown = progressive(() => lines().length);
  const visible = createMemo(() => lines().slice(0, shown()));

  async function copy() {
    const p = ready();
    if (!p) return;
    try {
      await navigator.clipboard.writeText(p.text);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      setCopied(false);
    }
  }

  return (
    <Switch>
      <Match when={state().status === "loading"}>
        <p class="drive-note" role="status">
          {t("drive.loading")}
        </p>
      </Match>
      <Match when={state().status === "pending"}>
        <Pending />
      </Match>
      <Match when={state().status === "empty"}>
        <p class="drive-note">{t("drive.noText")}</p>
      </Match>
      <Match when={state().status === "error"}>
        <p class="drive-note drive-note--error" role="alert">
          {t("drive.previewFailed")}
        </p>
      </Match>
      <Match when={ready()}>
        {(p) => (
          <div class="drive-text">
            <div class="drive-text__bar">
              <span class="drive-text__info">
                {[p().language ?? "", t("drive.lines", { count: lines().length }), formatBytes(p().bytes, locale())].filter(Boolean).join(" · ")}
              </span>
              <button type="button" class="drive-btn" onClick={() => void copy()}>
                <CopyIcon /> {copied() ? t("drive.copied") : t("drive.copy")}
              </button>
            </div>
            <Show when={p().truncated}>
              <p class="drive-note">{t("drive.truncated")}</p>
            </Show>
            <div class="drive-code" role="region" aria-label={t("drive.viewText")} tabIndex={0}>
              <For each={visible()}>
                {(line, i) => (
                  <div class="drive-code__line">
                    <span class="drive-code__no" aria-hidden="true">
                      {i() + 1}
                    </span>
                    <span class="drive-code__src">
                      <For each={line}>
                        {([cls, text]) => {
                          const c = tokenClass(cls);
                          return c ? <span class={c}>{text}</span> : text;
                        }}
                      </For>
                    </span>
                  </div>
                )}
              </For>
            </div>
          </div>
        )}
      </Match>
    </Switch>
  );
}

export function ArchiveView(props: { url: string; download: string }) {
  const [listing] = createResource(() => props.url, (url) => driveApi.archive(url));
  const entries = () => (listing.state === "ready" ? listing().entries : []);
  const shown = progressive(() => entries().length);
  const unsupported = () => listing.error instanceof ApiError && listing.error.status === 415;
  return (
    <Switch>
      <Match when={listing.loading}>
        <p class="drive-note" role="status">
          {t("drive.loading")}
        </p>
      </Match>
      <Match when={unsupported()}>
        <p class="drive-note">
          {t("drive.archiveUnsupported")}{" "}
          <a href={props.download} download="">
            {t("drive.download")}
          </a>
        </p>
      </Match>
      <Match when={listing.error}>
        <p class="drive-note drive-note--error" role="alert">
          {t("drive.previewFailed")}
        </p>
      </Match>
      <Match when={listing()}>
        {(l: Accessor<ArchiveListing>) => (
          <div class="drive-archive">
            <p class="drive-text__info">
              {[l().format ?? "", t("drive.archiveEntries", { count: l().total ?? l().entries.length })].filter(Boolean).join(" · ")}
            </p>
            <Show when={l().truncated}>
              <p class="drive-note">{t("drive.archiveTruncated", { count: l().entries.length })}</p>
            </Show>
            <table class="drive-archive__table">
              <thead>
                <tr>
                  <th scope="col">{t("drive.name")}</th>
                  <th scope="col">{t("drive.size")}</th>
                </tr>
              </thead>
              <tbody>
                <For each={entries().slice(0, shown())}>
                  {(e) => (
                    <tr data-dir={e.dir ? "true" : undefined}>
                      <td class="drive-archive__path">{e.path}</td>
                      <td class="drive-archive__size">{e.dir ? "" : formatBytes(e.size, locale())}</td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </div>
        )}
      </Match>
    </Switch>
  );
}

function InfoTable(props: { item: DriveItem }) {
  const rows = (): [string, string][] =>
    (
      [
        [t("drive.name"), itemName(props.item)],
        [t("drive.type"), t(`drive.${typeLabelKey(itemType(props.item))}`)],
        [t("drive.size"), formatBytes(props.item.size, locale())],
        [t("drive.modified"), formatModified(itemModified(props.item), locale())],
        [t("drive.location"), itemLocation(props.item)],
      ] as [string, string][]
    ).filter(([, v]) => v);
  return (
    <dl class="drive-info">
      <For each={rows()}>
        {([k, v]) => (
          <>
            <dt>{k}</dt>
            <dd>{v}</dd>
          </>
        )}
      </For>
    </dl>
  );
}

function asPlayerItem(item: DriveItem): PlayerItem {
  return {
    id: item.id,
    kind: itemPreview(item) === "audio" ? "audio" : "video",
    title: itemName(item),
    mime: item.mime || undefined,
    album: item.drive?.folder ?? item.album ?? undefined,
    size: item.size ?? undefined,
    duration_ms: item.durationMs,
    poster_url: item.posterUrl ?? undefined,
    thumb_url: item.thumbUrl ?? undefined,
  };
}

/** Share the file itself where the browser can (small files), else a link to it; copy the link otherwise. */
export async function shareItem(item: DriveItem): Promise<"shared" | "copied" | "cancelled" | "failed"> {
  const name = itemName(item);
  const link = new URL(fileHref(item.id), window.location.origin).href;
  const nav = navigator as Navigator & { share?: (d: ShareData) => Promise<void>; canShare?: (d: ShareData) => boolean };
  if (typeof nav.share === "function") {
    try {
      if (item.size !== null && item.size <= SHARE_FILE_LIMIT && typeof nav.canShare === "function") {
        const res = await fetch(downloadUrl(item), { credentials: "same-origin" });
        if (res.ok) {
          const blob = await res.blob();
          const file = new File([blob], name, { type: item.mime || blob.type || "application/octet-stream" });
          if (nav.canShare({ files: [file] })) {
            await nav.share({ files: [file], title: name });
            return "shared";
          }
        }
      }
      await nav.share({ title: name, url: link });
      return "shared";
    } catch (e) {
      if (e instanceof Error && e.name === "AbortError") return "cancelled";
    }
  }
  try {
    await navigator.clipboard.writeText(link);
    return "copied";
  } catch {
    return "failed";
  }
}

function PreviewBody(props: { item: DriveItem }) {
  const preview = () => itemPreview(props.item);
  const pdf = () => props.item.urls.pdf;
  const text = () => props.item.urls.text;
  const [asText, setAsText] = createSignal(false);
  const canToggle = () => !!pdf() && !!text() && (preview() === "pdf" || preview() === "office");
  const play = () => {
    void playQueue([asPlayerItem(props.item)], 0);
    media.setOpen(true);
  };
  return (
    <>
      <Show when={canToggle()}>
        <div class="drive-chips drive-chips--small" role="tablist" aria-label={t("drive.type")}>
          <button type="button" role="tab" class="media-chip" aria-selected={!asText()} onClick={() => setAsText(false)}>
            {t("drive.viewPdf")}
          </button>
          <button type="button" role="tab" class="media-chip" aria-selected={asText()} onClick={() => setAsText(true)}>
            {t("drive.viewText")}
          </button>
        </div>
      </Show>
      <Switch fallback={<InfoTable item={props.item} />}>
        <Match when={asText() && canToggle() && text()}>{(url) => <TextView url={url()} />}</Match>
        <Match when={(preview() === "pdf" || preview() === "office") && pdf()}>{(url) => <PdfView url={url()} />}</Match>
        <Match when={(preview() === "office" || preview() === "pdf" || preview() === "text" || preview() === "code") && text()}>
          {(url) => <TextView url={url()} />}
        </Match>
        <Match when={preview() === "archive" && props.item.urls.archive}>
          {(url) => <ArchiveView url={url()} download={downloadUrl(props.item)} />}
        </Match>
        <Match when={preview() === "image" && imageUrl(props.item)}>
          {(src) => <img class="drive-image" src={src()} alt={itemName(props.item)} />}
        </Match>
        <Match when={preview() === "video" || preview() === "audio"}>
          <div class="drive-play">
            <Show when={props.item.posterUrl ?? props.item.thumbUrl}>{(src) => <img class="drive-image" src={src()} alt="" />}</Show>
            <button type="button" class="drive-btn drive-btn--primary" onClick={play}>
              ▶ {t("drive.play")}
            </button>
          </div>
        </Match>
        <Match when={preview() !== "none"}>
          <p class="drive-note">{t("drive.noPreview")}</p>
          <InfoTable item={props.item} />
        </Match>
      </Switch>
    </>
  );
}

function PreviewDialog(props: { item: DriveItem | null; loading: boolean; failed: boolean; onClose: () => void }) {
  let closeBtn: HTMLButtonElement | undefined;
  const [shareNote, setShareNote] = createSignal("");
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape" && !media.open()) props.onClose();
  };
  onMount(() => {
    document.addEventListener("keydown", onKey);
    closeBtn?.focus();
  });
  onCleanup(() => document.removeEventListener("keydown", onKey));
  async function share(item: DriveItem) {
    const r = await shareItem(item);
    setShareNote(r === "copied" ? t("drive.linkCopied") : "");
    if (r === "copied") setTimeout(() => setShareNote(""), 2500);
  }
  return (
    <div class="drive-preview" role="dialog" aria-modal="true" aria-label={props.item ? itemName(props.item) : t("drive.loading")}>
      <div class="drive-preview__bar">
        <button type="button" class="media-icon-btn" aria-label={t("drive.close")} ref={closeBtn} onClick={() => props.onClose()}>
          <CloseIcon />
        </button>
        <Show when={props.item}>
          {(item) => (
            <>
              <FileGlyph type={itemType(item())} ext={item().drive?.ext} />
              <span class="drive-preview__title">
                <span class="drive-preview__name">{itemName(item())}</span>
                <span class="drive-row__meta">{[fileMeta(item()), itemLocation(item())].filter(Boolean).join(" · ")}</span>
              </span>
              <span class="drive-preview__actions">
                <Show when={item().urls.pdf && (itemPreview(item()) === "pdf" || itemPreview(item()) === "office")}>
                  <a class="drive-btn" href={item().urls.pdf} target="_blank" rel="noopener">
                    <ExternalIcon /> <span class="drive-btn__label">{t("drive.openNewTab")}</span>
                  </a>
                </Show>
                <a class="drive-btn" href={downloadUrl(item())} download={itemName(item())}>
                  <DownloadIcon /> <span class="drive-btn__label">{t("drive.download")}</span>
                </a>
                <button type="button" class="drive-btn" onClick={() => void share(item())}>
                  <ShareIcon /> <span class="drive-btn__label">{t("drive.share")}</span>
                </button>
              </span>
            </>
          )}
        </Show>
      </div>
      <Show when={shareNote()}>
        <p class="drive-toast" role="status">
          {shareNote()}
        </p>
      </Show>
      <div class="drive-preview__body">
        <Switch>
          <Match when={props.item}>{(item) => <PreviewBody item={item()} />}</Match>
          <Match when={props.loading}>
            <p class="drive-note" role="status">
              {t("drive.loading")}
            </p>
          </Match>
          <Match when={props.failed}>
            <p class="drive-note" role="alert">
              {t("drive.notFound")}
            </p>
          </Match>
        </Switch>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------- the view

const FILTERS: (DriveType | null)[] = [null, ...DRIVE_TYPES];

const SORT_KEYS: Record<DriveSort, "sortName" | "sortModified" | "sortSize" | "sortType"> = {
  name: "sortName",
  modified: "sortModified",
  size: "sortSize",
  type: "sortType",
};

export default function DriveView() {
  const measured = useWide();
  const wide = measured.wide;
  const location = useLocation<{ drivePreview?: boolean } | undefined>();
  const navigate = useNavigate();
  const route = createMemo(() => parseDriveRoute(location.pathname));
  const filters = createMemo(() => parseFilters(location.search), undefined, {
    equals: (a, b) => JSON.stringify(a) === JSON.stringify(b),
  });
  const openId = createMemo(() => {
    const r = route();
    return r.view === "file" ? r.id : openParam(location.search);
  });

  const [jxl, setJxl] = createSignal(false);
  onMount(() => void supportsJxl().then(setJxl));

  /** Every item seen in a listing, so opening one needs no request. */
  const known = new Map<string, DriveItem>();
  const [opened] = createResource(openId, async (id) => known.get(id) ?? (await driveApi.item(id)));

  const context = createMemo<Context | null>(
    () => {
      const r = route();
      if (r.view === "roots") return { kind: "roots" };
      if (r.view === "folder") return { kind: "folder", folder: r.folder, path: r.path };
      if (r.view === "album") return { kind: "album", id: r.id };
      if (opened.loading) return null;
      const d = opened.error ? null : opened()?.drive;
      return d ? { kind: "folder", folder: d.folder, path: d.dir } : { kind: "roots" };
    },
    null,
    { equals: sameContext },
  );

  // The roots (folders and albums), for the root view and album titles.
  const needRoots = createMemo(() => {
    const c = context();
    return !!c && c.kind !== "folder";
  });
  const [roots, { refetch: refetchRoots }] = createResource(
    () => needRoots() || undefined,
    () => driveApi.roots(),
  );

  // The listing: the current folder's subfolders and files, search results, or an album.
  const [folders, setFolders] = createSignal<DriveFolderEntry[]>([]);
  const [files, setFiles] = createSignal<DriveItem[]>([]);
  const [total, setTotal] = createSignal<number | null>(null);
  const [next, setNext] = createSignal<Next>(null);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal("");
  let token = 0;

  const searching = () => !!filters().q.trim();
  const rootSearch = () => context()?.kind === "roots" && (searching() || !!filters().type);

  async function fetchPage(c: Context, f: DriveFilters, after: Next) {
    if (c.kind === "folder") {
      const page = await driveApi.browse(c.folder, c.path, f, after && "offset" in after ? after.offset : 0);
      return {
        folders: page.folders,
        files: page.files,
        total: page.total,
        next: page.nextOffset !== null ? { offset: page.nextOffset } : null,
      };
    }
    const page = await driveApi.items({
      q: f.q,
      type: f.type,
      album: c.kind === "album" ? c.id : undefined,
      cursor: after && "cursor" in after ? after.cursor : null,
    });
    return { folders: [] as DriveFolderEntry[], files: page.items, total: page.total, next: page.nextCursor ? { cursor: page.nextCursor } : null };
  }

  async function load(more: boolean) {
    const c = context();
    if (!c) return;
    const f = filters();
    const mine = ++token;
    if (!more) {
      setFolders([]);
      setFiles([]);
      setNext(null);
      setTotal(null);
      setError("");
    }
    if (c.kind === "roots" && !f.q.trim() && !f.type) {
      setLoading(false);
      return;
    }
    setLoading(true);
    try {
      const page = await fetchPage(c, f, more ? next() : null);
      if (mine !== token) return;
      for (const item of page.files) known.set(item.id, item);
      if (more) {
        setFiles((prev) => [...prev, ...page.files]);
      } else {
        setFolders(page.folders);
        setFiles(page.files);
      }
      setTotal(page.total);
      setNext(page.next);
    } catch (e) {
      if (mine !== token) return;
      if (!more) {
        setFolders([]);
        setFiles([]);
      }
      setError(errorText(e));
    } finally {
      if (mine === token) setLoading(false);
    }
  }

  createEffect(
    on([context, filters], () => {
      void load(false);
    }),
  );

  // ---- navigation helpers

  const here = () => contextHref(context() ?? { kind: "roots" });

  function go(path: string, f: DriveFilters, opts: { replace?: boolean; open?: string | null; state?: unknown } = {}) {
    navigate(`${path}${filtersSearch(f, opts.open)}`, { replace: opts.replace ?? false, scroll: false, state: opts.state });
  }

  function openFolder(folder: string, path: string) {
    navigate(`${folderHref(folder, path)}${filtersSearch({ ...filters(), q: "" })}`);
  }

  function openFile(item: DriveItem) {
    known.set(item.id, item);
    go(here(), filters(), { open: item.id, state: { drivePreview: true } });
  }

  function closePreview() {
    if (location.state?.drivePreview) navigate(-1);
    else go(here(), filters(), { replace: true });
  }

  function setFilters(patch: Partial<DriveFilters>) {
    go(route().view === "file" ? here() : location.pathname, { ...filters(), ...patch }, { replace: true, open: route().view === "file" ? null : openParam(location.search) });
  }

  // ---- search box (debounced into ?q=)

  const [draft, setDraft] = createSignal(filters().q);
  let debounce: ReturnType<typeof setTimeout> | undefined;
  createEffect(
    on(
      () => filters().q,
      (q) => {
        if (draft().trim() !== q.trim()) setDraft(q);
      },
      { defer: true },
    ),
  );
  onCleanup(() => clearTimeout(debounce));
  function onSearch(value: string) {
    setDraft(value);
    clearTimeout(debounce);
    debounce = setTimeout(() => {
      if (value.trim() !== filters().q.trim()) setFilters({ q: value });
    }, 300);
  }

  // ---- labels

  const rootsData = () => (roots.error ? undefined : roots());
  const albumTitle = (id: string) => rootsData()?.albums.find((a) => a.id === id)?.title ?? id;

  const trail = createMemo((): { label: string; href: string }[] => {
    const c = context();
    if (!c || c.kind === "roots") return [];
    if (c.kind === "album") return [{ label: albumTitle(c.id), href: albumHref(c.id) }];
    return breadcrumbs(c.folder, c.path).map((cr) => ({ label: cr.name, href: folderHref(c.folder, cr.path) }));
  });

  const searchPlaceholder = () => {
    const tr = trail();
    return tr.length > 0 ? t("drive.searchIn", { name: tr[tr.length - 1]!.label }) : t("drive.search");
  };

  const showRoots = () => context()?.kind === "roots" && !rootSearch();
  const isFolder = () => context()?.kind === "folder";
  const showLocation = () => searching() || rootSearch();
  const listEmpty = () => !loading() && !error() && folders().length === 0 && files().length === 0;

  const contextPath = createMemo(() => {
    const tr = trail();
    return tr.length ? tr.map((c) => c.label).join(" › ") : undefined;
  });

  const typeChips = () => (
    <div class="drive-chips" role="group" aria-label={t("drive.filters")}>
      <For each={FILTERS}>
        {(type) => (
          <button
            type="button"
            class="media-chip"
            aria-pressed={filters().type === type ? "true" : "false"}
            onClick={() => setFilters({ type })}
          >
            {type ? t(`drive.${filterLabelKey(type)}`) : t("drive.filterAll")}
          </button>
        )}
      </For>
    </div>
  );

  const folderSort = () => (
    <Show when={isFolder()}>
      <div class="drive-sort">
        <SortIcon />
        <label class="media-select">
          <span class="sr-only">{t("drive.sortBy")}</span>
          <select
            aria-label={t("drive.sortBy")}
            value={filters().sort ?? (searching() ? "modified" : "name")}
            onChange={(e) => {
              const sort = e.currentTarget.value as DriveSort;
              setFilters({ sort, order: defaultOrder(sort) });
            }}
          >
            <For each={DRIVE_SORTS}>{(s) => <option value={s}>{t(`drive.${SORT_KEYS[s]}`)}</option>}</For>
          </select>
        </label>
        {(() => {
          const sort = () => filters().sort ?? (searching() ? "modified" : "name");
          const order = () => filters().order ?? defaultOrder(sort());
          return (
            <button
              type="button"
              class="drive-btn"
              aria-label={order() === "asc" ? t("drive.ascending") : t("drive.descending")}
              title={order() === "asc" ? t("drive.ascending") : t("drive.descending")}
              onClick={() => setFilters({ sort: sort(), order: order() === "asc" ? "desc" : "asc" })}
            >
              {order() === "asc" ? "↑" : "↓"}
            </button>
          );
        })()}
      </div>
    </Show>
  );

  const searchField = () => (
    <label class="media-search">
      <SearchIcon />
      <input
        type="search"
        class={wide() ? "pg-input" : undefined}
        placeholder={searchPlaceholder()}
        aria-label={searchPlaceholder()}
        value={draft()}
        onInput={(e) => onSearch(e.currentTarget.value)}
      />
    </label>
  );

  const deskRoots = () => (
    <Show when={rootsData()}>
      {(r) => (
        <>
          <Show when={r().folders.length + r().albums.length === 0}>
            <EmptyState text={t("drive.emptyDrive")} />
          </Show>
          <Show when={r().folders.length + r().albums.length > 0}>
            <Card list label={t("drive.title")}>
              <RowList>
                <For each={r().folders}>
                  {(f) => <DeskFolderRow name={f.folder} detail={folderDetail(f)} onOpen={() => openFolder(f.folder, "")} />}
                </For>
                <For each={r().albums}>
                  {(a) => (
                    <DeskFolderRow
                      name={a.title}
                      detail={a.count !== null ? t("drive.items", { count: a.count }) : ""}
                      onOpen={() => navigate(albumHref(a.id))}
                    />
                  )}
                </For>
              </RowList>
            </Card>
          </Show>
        </>
      )}
    </Show>
  );

  const deskBrowse = () => (
    <>
      <Show when={error()}>
        <ErrorState message={error()} retryLabel={t("common.tryAgain")} onRetry={() => void load(false)} />
      </Show>
      <Show when={(searching() || rootSearch()) && total() !== null && !loading()}>
        <p class="pg-context" role="status">
          {t("drive.results", { count: total() ?? 0 })}
        </p>
      </Show>
      <Show when={listEmpty() && !error()}>
        <EmptyState text={searching() || filters().type ? t("drive.noResults") : t("drive.emptyFolder")} />
      </Show>
      <Show when={folders().length > 0 || files().length > 0}>
        <Card list label={t("drive.title")}>
          <RowList>
            <For each={folders()}>
              {(f) => (
                <DeskFolderRow
                  name={f.name}
                  detail={folderDetail(f)}
                  onOpen={() => {
                    const c = context();
                    if (c?.kind === "folder") openFolder(c.folder, f.path);
                  }}
                />
              )}
            </For>
            <For each={files()}>{(item) => <DeskFileRow item={item} jxl={jxl()} showLocation={showLocation()} onOpen={() => openFile(item)} />}</For>
          </RowList>
        </Card>
      </Show>
      <Show when={loading()}>
        <LoadingState label={t("drive.loading")} />
      </Show>
      <Show when={next() && !loading()}>
        <Button class="drive-more" onClick={() => void load(true)}>
          {t("drive.loadMore")}
        </Button>
      </Show>
    </>
  );

  return (
    <>
      <div ref={measured.track}>
        <Show
          when={wide()}
          fallback={
            <section class="media drive" aria-labelledby="drive-title">
              <A href="/plugins" class="media-back">
                <BackIcon /> {t("drive.back")}
              </A>
              <h1 id="drive-title">{t("drive.title")}</h1>

              <nav class="drive-crumbs" aria-label={t("drive.location")}>
                <ol>
                  <li>
                    <A href="/drive" aria-current={trail().length === 0 ? "page" : undefined}>
                      {t("drive.title")}
                    </A>
                  </li>
                  <For each={trail()}>
                    {(c, i) => (
                      <li>
                        <span class="drive-crumbs__sep" aria-hidden="true">
                          ›
                        </span>
                        <A href={c.href} aria-current={i() === trail().length - 1 ? "page" : undefined}>
                          {c.label}
                        </A>
                      </li>
                    )}
                  </For>
                </ol>
              </nav>

              <div class="media-tools drive-tools">
                {searchField()}
                {folderSort()}
              </div>

              {typeChips()}

              <Show when={showRoots()}>
                <Show when={roots.error}>
                  <p class="media-muted" role="alert">
                    {errorText(roots.error)}
                  </p>
                </Show>
                <Show when={roots.loading}>
                  <p class="media-muted" role="status">
                    {t("drive.loading")}
                  </p>
                </Show>
                <Show when={rootsData()}>
                  {(r) => (
                    <>
                      <Show when={r().folders.length > 0}>
                        <h2 class="media-section">{t("drive.folders")}</h2>
                        <ul class="drive-list">
                          <For each={r().folders}>{(f) => <FolderRow name={f.folder} detail={folderDetail(f)} onOpen={() => openFolder(f.folder, "")} />}</For>
                        </ul>
                      </Show>
                      <Show when={r().albums.length > 0}>
                        <h2 class="media-section">{t("drive.collections")}</h2>
                        <ul class="drive-list">
                          <For each={r().albums}>
                            {(a) => (
                              <FolderRow
                                name={a.title}
                                detail={a.count !== null ? t("drive.items", { count: a.count }) : ""}
                                onOpen={() => navigate(albumHref(a.id))}
                              />
                            )}
                          </For>
                        </ul>
                      </Show>
                      <Show when={r().folders.length === 0 && r().albums.length === 0}>
                        <p class="media-muted">{t("drive.emptyDrive")}</p>
                      </Show>
                    </>
                  )}
                </Show>
              </Show>

              <Show when={!showRoots() && context()}>
                <Show when={error()}>
                  <p class="media-muted" role="alert">
                    {error()}
                  </p>
                </Show>
                <Show when={(searching() || rootSearch()) && total() !== null && !loading()}>
                  <p class="media-muted" role="status">
                    {t("drive.results", { count: total() ?? 0 })}
                  </p>
                </Show>
                <Show when={folders().length > 0 || files().length > 0}>
                  <ul class="drive-list">
                    <For each={folders()}>
                      {(f) => (
                        <FolderRow
                          name={f.name}
                          detail={folderDetail(f)}
                          onOpen={() => {
                            const c = context();
                            if (c?.kind === "folder") openFolder(c.folder, f.path);
                          }}
                        />
                      )}
                    </For>
                    <For each={files()}>{(item) => <FileRow item={item} jxl={jxl()} showLocation={showLocation()} onOpen={() => openFile(item)} />}</For>
                  </ul>
                </Show>
                <Show when={listEmpty()}>
                  <p class="media-muted">{searching() || filters().type ? t("drive.noResults") : t("drive.emptyFolder")}</p>
                </Show>
                <Show when={loading()}>
                  <p class="media-muted" role="status">
                    {t("drive.loading")}
                  </p>
                </Show>
                <Show when={next() && !loading()}>
                  <button type="button" class="media-text-btn drive-more" onClick={() => void load(true)}>
                    {t("drive.loadMore")}
                  </button>
                </Show>
              </Show>
            </section>
          }
        >
          <Page class="drive-desk">
            <PageHeader title={t("drive.title")} context={contextPath()} action={searchField()} />
            <PageGrid layout="single">
              {typeChips()}
              {folderSort()}
              <Show when={showRoots()}>
                <Show when={roots.error}>
                  <ErrorState message={errorText(roots.error)} retryLabel={t("common.tryAgain")} onRetry={() => void refetchRoots()} />
                </Show>
                <Show when={roots.loading && !rootsData()}>
                  <LoadingState label={t("drive.loading")} />
                </Show>
                {deskRoots()}
              </Show>
              <Show when={!showRoots() && context()}>{deskBrowse()}</Show>
            </PageGrid>
          </Page>
        </Show>
      </div>

      <Show when={openId()}>
        <PreviewDialog item={opened.error ? null : (opened() ?? null)} loading={opened.loading} failed={!!opened.error || (!opened.loading && !opened())} onClose={closePreview} />
      </Show>

      <MiniPlayer />
      <Show when={media.open() && media.current()}>
        <Player />
      </Show>
    </>
  );
}

