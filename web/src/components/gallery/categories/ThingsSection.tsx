import { createResource, For, Show } from "solid-js";
import { listCategories } from "../../../api/gallery";
import { t } from "../../../i18n";
import { supportsJxl } from "../../../lib/jxl";
import { ThingTile } from "./ThingTile";
import "../../../styles/gallery-categories.css";

/**
 * "Things" on the albums page: the automatic albums (screenshots, receipts, documents…) the server found by itself. They are
 * views of the Gallery, not albums: no card to rename, delete or share, and the photos stay where they are. The section is
 * supplementary, so it shows nothing until the server has put a photo in one, and nothing when the server cannot say.
 */
export function ThingsSection() {
  const [albums] = createResource(() => listCategories().catch(() => []));
  const [jxl] = createResource(supportsJxl);
  return (
    <Show when={(albums() ?? []).length > 0}>
      <section class="cat-things" aria-labelledby="cat-things-title">
        <h2 class="al-section" id="cat-things-title">
          {t("galleryCategories.title")}
        </h2>
        <p class="cat-note">{t("galleryCategories.hint")}</p>
        <ul class="al-grid al-grid--small" aria-label={t("galleryCategories.title")}>
          <For each={albums()}>
            {(album) => (
              <li>
                <ThingTile album={album} jxl={jxl() ?? false} />
              </li>
            )}
          </For>
        </ul>
      </section>
    </Show>
  );
}
