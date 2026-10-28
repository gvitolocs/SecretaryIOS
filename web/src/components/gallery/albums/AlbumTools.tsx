import { createSignal, Show, type JSX } from "solid-js";
import type { AlbumCard } from "../../../api/gallery";
import { t } from "../../../i18n";
import { isBuiltIn } from "../../../lib/galleryAlbums";
import { TrashIcon } from "../../Icons";
import { ShareSheet } from "../share/ShareSheet";
import { DeleteAlbumDialog, RenameAlbumDialog } from "./AlbumDialogs";
import { RenamePenIcon, ShareIcon } from "./AlbumIcons";
import "../../../styles/gallery-albums.css";

type Dialog = "rename" | "delete" | "share" | null;

export interface AlbumToolsProps {
  /** Header icon buttons of the page system (desktop). */
  wide?: boolean;
  album: AlbumCard;
  onRenamed: (album: AlbumCard) => void;
  /** The album is gone (its photos are not). */
  onDeleted: () => void;
}

/** The buttons in an album's header: share, rename and delete (the built-in albums only share). */
export function AlbumTools(props: AlbumToolsProps) {
  const [dialog, setDialog] = createSignal<Dialog>(null);
  const button = (label: string, icon: () => JSX.Element, open: Dialog) => (
    <button type="button" class={props.wide ? "pg-icon-btn" : "g-iconbtn"} aria-label={label} title={label} onClick={() => setDialog(open)}>
      {icon()}
    </button>
  );
  return (
    <>
      {button(t("galleryAlbums.share"), () => <ShareIcon />, "share")}
      <Show when={!isBuiltIn(props.album)}>
        {button(t("galleryAlbums.rename"), () => <RenamePenIcon />, "rename")}
        {button(t("galleryAlbums.deleteAlbum"), () => <TrashIcon />, "delete")}
      </Show>
      <Show when={dialog() === "rename"}>
        <RenameAlbumDialog
          album={props.album}
          onRenamed={(album) => {
            setDialog(null);
            props.onRenamed(album);
          }}
          onCancel={() => setDialog(null)}
        />
      </Show>
      <Show when={dialog() === "delete"}>
        <DeleteAlbumDialog album={props.album} onDeleted={props.onDeleted} onCancel={() => setDialog(null)} />
      </Show>
      <Show when={dialog() === "share"}>
        <ShareSheet target={{ kind: "album", id: props.album.id, title: props.album.title }} onClose={() => setDialog(null)} />
      </Show>
    </>
  );
}
