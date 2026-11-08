import { useNavigate } from "@solidjs/router";
import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { t } from "../i18n";
import { handOff } from "../lib/chatHandoff";
import { copyFromUrl, fileFromUrl } from "../lib/copyImage";
import { desktopShell } from "../lib/desktopShell";
import { ContextMenu, menuMode, type MenuAction, type MenuRequest } from "./ContextMenu";
import { ChatIcon, CopyIcon, DownloadIcon, LinkIcon, ShareIcon } from "./Icons";
import "../styles/context-menu.css";

// One context menu for every picture the app shows (Pixiv, Instagram, NClient, Films posters, profile pictures,
// the viewers): a right-click, a Ctrl-click or the context-menu key on a picture that no page menu took (the Gallery
// has its own) offers Copy image, Copy link, Download, Share and "Ask Lucina". A page can say more about a picture
// with data attributes on it or around it: data-media-src (the full-size picture, for an element that stands for one),
// data-media-link (the page it belongs to, e.g. the Pixiv work or the Instagram post) and data-media-name.

export interface Media {
  src: string;
  link?: string;
  name?: string;
}

/** Pictures smaller than this (in their own pixels) are icons, not media. */
const MIN_PICTURE = 64;

/** The picture a right-click at `el` is about, or null (not a picture, an icon, text being selected, a field). */
export function mediaAt(el: Element | null): Media | null {
  if (!el || el.closest("input, textarea, select, [contenteditable=''], [contenteditable='true'], [data-media-off]")) return null;
  const hinted = el.closest<HTMLElement>("[data-media-src]");
  const box = el.closest("button, a, li, figure, [role=listitem], .pv-slot, .px-stage");
  const img = (el instanceof HTMLImageElement ? el : null) ?? box?.querySelector<HTMLImageElement>("img") ?? null;
  const video = el.closest("video") ?? box?.querySelector("video") ?? null;
  let src = hinted?.dataset.mediaSrc;
  if (!src && img && (img.naturalWidth || 0) >= MIN_PICTURE) src = img.currentSrc || img.src;
  if (!src && video?.poster) src = video.poster;
  if (!src || src.startsWith("data:image/svg")) return null;
  const linked = el.closest<HTMLElement>("[data-media-link]")?.dataset.mediaLink;
  const anchor = el.closest<HTMLAnchorElement>("a[href]");
  const name = el.closest<HTMLElement>("[data-media-name]")?.dataset.mediaName ?? (img?.alt || undefined);
  return { src, link: linked ?? (anchor && !anchor.href.startsWith("javascript:") ? anchor.href : undefined), name };
}

/** A file name for a picture: its own name, else the URL's last part. */
function fileName(media: Media): string {
  const base = media.name?.trim() || new URL(media.src, location.href).pathname.split("/").filter(Boolean).pop() || "picture";
  return /\.[A-Za-z0-9]{2,5}$/.test(base) ? base : `${base}.jpg`;
}

export function MediaMenuHost() {
  const navigate = useNavigate();
  const [open, setOpen] = createSignal<{ request: MenuRequest; media: Media } | null>(null);
  const [toast, setToast] = createSignal<{ text: string; error: boolean } | null>(null);
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  const say = (text: string, error = false) => {
    clearTimeout(toastTimer);
    setToast({ text, error });
    toastTimer = setTimeout(() => setToast(null), error ? 6000 : 3000);
  };

  // On the window, after every page's own handlers: a page menu (the Gallery's) has already said preventDefault.
  const onContextMenu = (e: MouseEvent) => {
    if (e.defaultPrevented) return;
    const target = e.target as Element | null;
    const selected = window.getSelection()?.toString();
    if (selected && target && window.getSelection()?.containsNode(target, true)) return;
    const media = mediaAt(target);
    if (!media) return;
    e.preventDefault();
    const keyboard = e.clientX === 0 && e.clientY === 0;
    const r = keyboard ? (target as HTMLElement).getBoundingClientRect() : null;
    const at = r ? { x: r.left + r.width / 2, y: r.top + r.height / 2 } : { x: e.clientX, y: e.clientY };
    const pointer = (e as PointerEvent).pointerType;
    setOpen({ request: { ...at, mode: menuMode(pointer === "touch" ? "touch" : "mouse"), title: media.name }, media });
  };
  onMount(() => window.addEventListener("contextmenu", onContextMenu));
  onCleanup(() => {
    window.removeEventListener("contextmenu", onContextMenu);
    clearTimeout(toastTimer);
  });

  const actions = (media: Media, sheet: boolean): MenuAction[] => {
    const absolute = (u: string) => new URL(u, location.href).href;
    const list: MenuAction[] = [
      {
        id: "copyImage",
        label: t("media.copyImage"),
        icon: <CopyIcon />,
        // Straight from the click: the clipboard write starts before anything is awaited (Safari needs that).
        run: () => void copyFromUrl(media.src).then((ok) => say(ok ? t("media.copiedImage") : t("media.copyFailed"), !ok)),
      },
      {
        id: "copyLink",
        label: t("media.copyLink"),
        icon: <LinkIcon />,
        run: () =>
          void navigator.clipboard
            ?.writeText(absolute(media.link ?? media.src))
            .then(() => say(t("media.copiedLink")))
            .catch(() => say(t("media.copyFailed"), true)),
      },
      {
        id: "download",
        label: t("media.download"),
        icon: <DownloadIcon />,
        run: () => {
          const a = document.createElement("a");
          a.href = media.src;
          a.download = fileName(media);
          a.rel = "noopener";
          document.body.append(a);
          a.click();
          a.remove();
        },
      },
    ];
    if (sheet && typeof navigator.share === "function") {
      list.push({
        id: "share",
        label: t("media.share"),
        icon: <ShareIcon />,
        run: () =>
          void fileFromUrl(media.src, fileName(media))
            .then((file) => (file && navigator.canShare?.({ files: [file] }) ? navigator.share({ files: [file] }) : navigator.share({ url: absolute(media.link ?? media.src) })))
            .catch(() => {}),
      });
    }
    list.push({
      id: "askLucina",
      label: t("media.askLucina"),
      icon: <ChatIcon />,
      separator: true,
      run: () =>
        void fileFromUrl(media.src, fileName(media))
          .then((file) => {
            if (!file) throw new Error("no picture");
            handOff([file]);
            desktopShell()?.show?.("lucina");
            navigate("/");
          })
          .catch(() => say(t("media.askLucinaFailed"), true)),
    });
    return list;
  };

  return (
    <>
      <ContextMenu
        request={open()?.request ?? null}
        actions={(() => {
          const o = open();
          return o ? actions(o.media, o.request.mode === "sheet") : [];
        })()}
        label={t("media.menuLabel")}
        onClose={() => setOpen(null)}
      />
      <Show when={toast()}>
        {(m) => (
          <p class="mm-toast" classList={{ "mm-toast--error": m().error }} role={m().error ? "alert" : "status"}>
            {m().text}
          </p>
        )}
      </Show>
    </>
  );
}
