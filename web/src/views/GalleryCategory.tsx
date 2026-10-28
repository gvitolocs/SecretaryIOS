import { A, useParams } from "@solidjs/router";
import { Show } from "solid-js";
import { Card, EmptyState, Page, PageGrid, useWide } from "../components/page";
import { GalleryDetail, GalleryHeader } from "../components/gallery/GalleryHeader";
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
  const { wide, track } = useWide();
  // Another category is another list: start the view afresh.
  return (
    <Show
      when={categoryFromParam(params.cat)}
      keyed
      fallback={
        <div class={wide() ? "g-page-root" : "al-page"} ref={track}>
          <Show
            when={wide()}
            fallback={
              <>
                <header class="al-head">
                  <A class="g-back" href={ALBUMS_PATH} aria-label={t("galleryCategories.backToAlbums")}>
                    <BackIcon />
                  </A>
                  <h1 class="al-h1">{t("galleryCategories.title")}</h1>
                </header>
                <p class="al-empty">{t("galleryCategories.pageMissing")}</p>
              </>
            }
          >
            <Page class="g-lib">
              <GalleryHeader />
              <GalleryDetail href={ALBUMS_PATH} back={t("galleryAlbums.title")} title={t("galleryCategories.title")} />
              <PageGrid layout="full">
                <Card label={t("galleryCategories.title")}>
                  <EmptyState text={t("galleryCategories.pageMissing")} />
                </Card>
              </PageGrid>
            </Page>
          </Show>
        </div>
      }
    >
      {(category) => <GalleryView category={category} desktop />}
    </Show>
  );
}
