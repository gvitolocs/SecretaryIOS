import { A } from "@solidjs/router";
import { Show } from "solid-js";
import { tileUrl, type CategoryAlbum } from "../../../api/gallery";
import { t } from "../../../i18n";
import { albumKey, categoryPath } from "../../../lib/galleryCategories";
import { thumbHashToImage } from "../../../lib/thumbhash";
import { CategoryIcon } from "./CategoryIcons";

/** One automatic album on the albums page: the newest photo of the category, its name and the photo count. */
export function ThingTile(props: { album: CategoryAlbum; jxl: boolean }) {
  const title = () => t(albumKey(props.album.category));
  const hash = () => thumbHashToImage(props.album.cover?.thumbhash ?? undefined)?.url ?? null;
  return (
    <A class="al-tile cat-tile" href={categoryPath(props.album.category)} aria-label={t("galleryCategories.open", { title: title() })}>
      <span class="al-cover" style={hash() ? { "background-image": `url(${hash()})` } : undefined}>
        <Show when={props.album.cover} fallback={<CategoryIcon category={props.album.category} />}>
          {(item) => (
            <Show when={tileUrl(item(), props.jxl)}>
              {(src) => <img class="al-cover__img" src={src()} alt="" draggable={false} loading="lazy" style={item().rotation ? { transform: `rotate(${item().rotation}deg)` } : undefined} />}
            </Show>
          )}
        </Show>
        <span class="cat-badge">
          <CategoryIcon category={props.album.category} />
        </span>
      </span>
      <span class="al-title">{title()}</span>
      <span class="al-count">{t("galleryCategories.count", { count: props.album.count })}</span>
    </A>
  );
}
