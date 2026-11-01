import type { GalleryItem } from "../api/gallery";
import type { MenuAction } from "../components/ContextMenu";
import { t } from "../i18n";

// What the Gallery's context menu offers for the photos it was opened on (one tile, the selection, or the viewer's
// photo), in order. Pure: the page passes its own actions in `env`, so the list itself is testable.

export interface GalleryMenuEnv {
  /** In the viewer: no Open. */
  viewer?: boolean;
  /** The phone sheet: offers Select. */
  sheet?: boolean;
  trash?: boolean;
  archive?: boolean;
  /** Whether `item` is selected already (Select is for one that is not). */
  isSelected?: (item: GalleryItem) => boolean;
  icons?: Partial<Record<string, MenuAction["icon"]>>;
  open: (item: GalleryItem) => void;
  copyImage: (item: GalleryItem) => void;
  copyLink: (items: GalleryItem[]) => void;
  download: (items: GalleryItem[]) => void;
  share: (items: GalleryItem[]) => void;
  favorite: (items: GalleryItem[], make: boolean) => void;
  archiveToggle?: (items: GalleryItem[]) => void;
  restore?: (items: GalleryItem[]) => void;
  showDay: (item: GalleryItem) => void;
  askLucina: (item: GalleryItem) => void;
  select: (item: GalleryItem) => void;
  remove: (items: GalleryItem[]) => void;
}

/** The menu's title on the phone sheet: the photo's name, or how many. */
export function galleryMenuTitle(targets: GalleryItem[]): string {
  if (targets.length !== 1) return t("gallery.selected", { count: targets.length });
  const one = targets[0]!;
  return one.title || one.origin.filename || "";
}

export function galleryActions(targets: GalleryItem[], env: GalleryMenuEnv): MenuAction[] {
  if (!targets.length) return [];
  const one = targets.length === 1 ? targets[0]! : null;
  const icon = (id: string) => env.icons?.[id];
  const out: MenuAction[] = [];
  const add = (id: string, label: string, run: () => void, extra: Partial<MenuAction> = {}) => out.push({ id, label, icon: icon(id), run, ...extra });

  if (one && !env.viewer) add("open", t("gallery.menuOpen"), () => env.open(one));
  if (env.trash) {
    // The Trash keeps what it allows today: restore it, or delete it for good.
    add("copyLink", targets.length > 1 ? t("gallery.menuCopyLinks") : t("gallery.menuCopyLink"), () => env.copyLink(targets));
    if (env.restore) add("restore", t("gallery.restore"), () => env.restore!(targets));
    add("delete", t("gallery.deleteForever"), () => env.remove(targets), { danger: true, separator: true });
    return out;
  }
  if (one?.kind === "photo") add("copyImage", t("gallery.menuCopyImage"), () => env.copyImage(one));
  if (one?.kind === "video" && one.urls.poster) add("copyImage", t("gallery.menuCopyFrame"), () => env.copyImage(one));
  add("copyLink", targets.length > 1 ? t("gallery.menuCopyLinks") : t("gallery.menuCopyLink"), () => env.copyLink(targets));
  add("download", t("gallery.menuDownload"), () => env.download(targets), { disabled: !targets.some((i) => i.urls.download ?? i.urls.original) });
  add("share", t("gallery.share"), () => env.share(targets));
  if (env.archive) {
    if (env.archiveToggle) add("archive", t("browse.unarchive"), () => env.archiveToggle!(targets));
  } else {
    const all = targets.every((i) => i.favorite);
    add("favorite", all ? t("gallery.unfavorite") : t("gallery.favorite"), () => env.favorite(targets, !all));
  }
  if (one) add("showDay", t("gallery.menuShowDay"), () => env.showDay(one), { separator: true });
  if (one?.kind === "photo") add("askLucina", t("gallery.menuAskLucina"), () => env.askLucina(one), { separator: !one });
  if (one && env.sheet && !env.isSelected?.(one)) add("select", t("gallery.select"), () => env.select(one));
  add("delete", t("gallery.delete"), () => env.remove(targets), { danger: true, separator: true });
  return out;
}
