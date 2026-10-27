import { Show, type JSX } from "solid-js";
import { CloseIcon, DownloadIcon, RefreshIcon, TrashIcon } from "../Icons";
import { t } from "../../i18n";

export interface SelectionBarProps {
  count: number;
  /** Every loaded item is selected already. */
  all: boolean;
  /** Every selected item is a favorite (the button then removes them). */
  allFavorite: boolean;
  /** The Trash view: Restore and Delete forever instead of Favorite, Album, Download and Delete. */
  trash?: boolean;
  busy: boolean;
  onSelectAll: () => void;
  onClear: () => void;
  onFavorite: () => void;
  onAlbum: () => void;
  onDownload: () => void;
  onDelete: () => void;
  onRestore: () => void;
}

function HeartIcon(props: { filled: boolean }) {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true" style={props.filled ? { fill: "currentColor" } : undefined}>
      <path d="M12 20.4s-7.5-4.6-7.5-10.2A4.3 4.3 0 0 1 12 7.6a4.3 4.3 0 0 1 7.5 2.6c0 5.6-7.5 10.2-7.5 10.2z" />
    </svg>
  );
}

function AlbumIcon() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="3.5" y="6" width="14" height="14" rx="2.2" />
      <path d="M7 3.5h11a2.5 2.5 0 0 1 2.5 2.5v11M10.5 10.5v6M7.5 13.5h6" />
    </svg>
  );
}

/** What can be done with the selected pictures; it takes the place of the search row while something is selected. */
export function SelectionBar(props: SelectionBarProps) {
  const act = (label: string, icon: () => JSX.Element, run: () => void, danger = false) => (
    <button type="button" class="g-act" classList={{ "g-act--danger": danger }} disabled={props.busy} aria-label={label} title={label} onClick={run}>
      {icon()}
      <span class="g-act__label">{label}</span>
    </button>
  );
  return (
    <div class="g-selbar" role="toolbar" aria-label={t("gallery.selectionActions")}>
      <button type="button" class="g-act g-act--icon" aria-label={t("gallery.clearSelection")} title={t("gallery.clearSelection")} onClick={props.onClear}>
        <CloseIcon />
      </button>
      <span class="g-selbar__count" role="status" aria-live="polite">
        {t("gallery.selected", { count: props.count })}
      </span>
      <Show when={!props.all}>
        <button type="button" class="g-selbar__all" onClick={props.onSelectAll}>
          {t("gallery.selectAll")}
        </button>
      </Show>
      <span class="g-selbar__spacer" />
      <Show
        when={!props.trash}
        fallback={
          <>
            {act(t("gallery.restore"), () => <RefreshIcon />, props.onRestore)}
            {act(t("gallery.deleteForever"), () => <TrashIcon />, props.onDelete, true)}
          </>
        }
      >
        {act(props.allFavorite ? t("gallery.unfavorite") : t("gallery.favorite"), () => <HeartIcon filled={props.allFavorite} />, props.onFavorite)}
        {act(t("gallery.addToAlbum"), () => <AlbumIcon />, props.onAlbum)}
        {act(t("gallery.download"), () => <DownloadIcon />, props.onDownload)}
        {act(t("gallery.delete"), () => <TrashIcon />, props.onDelete, true)}
      </Show>
    </div>
  );
}
