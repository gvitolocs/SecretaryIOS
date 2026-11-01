import { A } from "@solidjs/router";
import { createResource, Show } from "solid-js";
import { getItem, listGallery, tileUrl, type AlbumCard, type GalleryItem } from "../../../api/gallery";
import { t } from "../../../i18n";
import { albumPath } from "../../../lib/galleryAlbums";
import { thumbHashToImage } from "../../../lib/thumbhash";
import { AlbumIcon } from "../ViewerIcons";

/** The picture that stands for an album: its chosen cover, else its newest photo; null for an empty album. */
async function coverOf(album: AlbumCard): Promise<GalleryItem | null> {
  try {
    if (album.coverItem) {
      const cover = await getItem(album.coverItem);
      if (cover) return cover;
    }
    if (album.count > 0) return (await listGallery({ album: album.id, limit: 1 })).items[0] ?? null;
  } catch {
    // No cover: the card shows the album mark instead.
  }
  return null;
}

/** One album on the albums page: the cover (ThumbHash first, then the thumbnail), the title and the photo count. */
export function AlbumTile(props: { album: AlbumCard; jxl: boolean }) {
  const [cover] = createResource(() => props.album.id, () => coverOf(props.album));
  const title = () => props.album.title || t("galleryAlbums.untitled");
  const hash = () => thumbHashToImage(cover()?.thumbhash ?? undefined)?.url ?? null;
  return (
    <A class="al-tile" href={albumPath(props.album.id)} aria-label={t("galleryAlbums.open", { title: title() })}>
      <span class="al-cover" style={hash() ? { "background-image": `url(${hash()})` } : undefined}>
        <Show when={cover()} fallback={<AlbumIcon />}>
          {(item) => (
            <Show when={tileUrl(item(), props.jxl)}>
              {(src) => <img class="al-cover__img" src={src()} alt="" draggable={false} loading="lazy" style={item().rotation ? { transform: `rotate(${item().rotation}deg)` } : undefined} />}
            </Show>
          )}
        </Show>
      </span>
      <span class="al-title">{title()}</span>
      <span class="al-count">{t("galleryAlbums.count", { count: props.album.count })}</span>
    </A>
  );
}
