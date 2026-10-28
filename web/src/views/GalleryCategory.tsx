import { A, useParams } from "@solidjs/router";
import { Show } from "solid-js";
import { BackIcon } from "../components/Icons";
import { t } from "../i18n";
import { ALBUMS_PATH } from "../lib/galleryAlbums";
import { categoryFromParam } from "../lib/galleryCategories";
import GalleryView from "./Gallery";
import "../styles/gallery-albums.css";

/**
 * /gallery/category/:cat: the Gallery's own grid and viewer over one automatic album (screenshots, receipts…). It is a view
 * of the Gallery, so the photos can be opened, favourited or trashed as anywhere, but not renamed, deleted or shared as an album.
 */
export default function GalleryCategoryView() {
  const params = useParams<{ cat: string }>();
  // Another category is another list: start the view afresh.
  return (
    <Show
      when={categoryFromParam(params.cat)}
      keyed
      fallback={
        <div class="al-page">
          <header class="al-head">
            <A class="g-back" href={ALBUMS_PATH} aria-label={t("galleryCategories.backToAlbums")}>
              <BackIcon />
            </A>
            <h1 class="al-h1">{t("galleryCategories.title")}</h1>
          </header>
          <p class="al-empty">{t("galleryCategories.pageMissing")}</p>
        </div>
      }
    >
      {(category) => <GalleryView category={category} />}
    </Show>
  );
}
