import { A, useNavigate } from "@solidjs/router";
import { createMemo, createResource, createSignal, For, Show } from "solid-js";
import { getCategoryStatus, listAlbumCards } from "../api/gallery";
import { NewAlbumDialog } from "../components/gallery/albums/AlbumDialogs";
import { AlbumTile } from "../components/gallery/albums/AlbumTile";
import { ThingsSection } from "../components/gallery/categories/ThingsSection";
import { BackIcon, PlusIcon } from "../components/Icons";
import { t } from "../i18n";
import { albumPath, sectionAlbums } from "../lib/galleryAlbums";
import { supportsJxl } from "../lib/jxl";
import "../styles/gallery-albums.css";

/**
 * /gallery/albums: the albums as cards (cover, name, photo count), the owner's first and the built-in ones (Lucina's)
 * in a small section after. The hidden albums behind photo-selection share links are never listed.
 */
export default function GalleryAlbumsView() {
  const navigate = useNavigate();
  const [albums, { refetch }] = createResource(listAlbumCards);
  const [jxl] = createResource(supportsJxl);
  const [creating, setCreating] = createSignal(false);
  // Things replaces the built-in Screenshots album once the server has decided categories; any failure keeps the old album.
  const [things] = createResource(() =>
    getCategoryStatus()
      .then((s) => s.enabled && s.decided > 0)
      .catch(() => false),
  );
  // Reading a failed resource throws: the list is empty then, and the error shows below.
  const sections = createMemo(() => sectionAlbums(albums.error ? [] : (albums() ?? []), { things: things() === true }));

  return (
    <div class="al-page">
      <header class="al-head">
        <A class="g-back" href="/gallery" aria-label={t("gallery.backToGallery")}>
          <BackIcon />
        </A>
        <h1 class="al-h1">{t("galleryAlbums.title")}</h1>
        <button type="button" class="btn btn--primary" onClick={() => setCreating(true)}>
          <PlusIcon /> {t("galleryAlbums.newAlbum")}
        </button>
      </header>

      <Show when={!albums.loading} fallback={<p class="al-empty" role="status">{t("gallery.loading")}</p>}>
        <Show
          when={!albums.error}
          fallback={
            <p class="al-empty" role="alert">
              {t("galleryAlbums.loadError")}{" "}
              <button type="button" class="btn" onClick={() => void refetch()}>
                {t("galleryAlbums.retry")}
              </button>
            </p>
          }
        >
          <Show when={sections().own.length > 0} fallback={<Show when={sections().builtIn.length === 0}><p class="al-empty">{t("galleryAlbums.empty")}<br />{t("galleryAlbums.emptyHint")}</p></Show>}>
            <ul class="al-grid" aria-label={t("galleryAlbums.title")}>
              <For each={sections().own}>{(album) => <li><AlbumTile album={album} jxl={jxl() ?? false} /></li>}</For>
            </ul>
          </Show>
          <Show when={sections().builtIn.length > 0}>
            <h2 class="al-section">{t("galleryAlbums.builtIn")}</h2>
            <ul class="al-grid al-grid--small" aria-label={t("galleryAlbums.builtIn")}>
              <For each={sections().builtIn}>{(album) => <li><AlbumTile album={album} jxl={jxl() ?? false} /></li>}</For>
            </ul>
          </Show>
        </Show>
        <ThingsSection />
      </Show>

      <Show when={creating()}>
        <NewAlbumDialog onCreated={(album) => navigate(albumPath(album.id))} onCancel={() => setCreating(false)} />
      </Show>
    </div>
  );
}
