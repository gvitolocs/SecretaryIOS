import { createResource, createSignal, For, Show } from "solid-js";
import { listGallery, tileUrl } from "../api/gallery";
import { GalleryViewer } from "../components/gallery/GalleryViewer";
import { t } from "../i18n";
import { supportsJxl } from "../lib/jxl";

/** The Gallery (/gallery, and /media for the Mac shell's rail): the grid builder replaces this first page. */
export default function GalleryView(props: { trash?: boolean }) {
  const [page, { mutate }] = createResource(() => listGallery({ limit: 200, trashed: props.trash }));
  const [jxl] = createResource(supportsJxl);
  const [open, setOpen] = createSignal<number | null>(null);
  const items = () => page()?.items ?? [];
  return (
    <section class="gallery" aria-label={t("gallery.title")}>
      <Show when={!page.error} fallback={<p role="alert">{t("gallery.loadError")}</p>}>
        <Show when={!page.loading || items().length} fallback={<p>{t("gallery.loading")}</p>}>
          <Show when={items().length} fallback={<p>{t("gallery.empty")}</p>}>
            <div class="gallery-grid">
              <For each={items()}>
                {(item, i) => (
                  <button type="button" class="gallery-tile" aria-label={item.title} onClick={() => setOpen(i())}>
                    <img src={tileUrl(item, jxl() ?? false) ?? undefined} alt="" loading="lazy" />
                  </button>
                )}
              </For>
            </div>
          </Show>
        </Show>
      </Show>
      <Show when={open() !== null}>
        <GalleryViewer
          items={items()}
          index={open()!}
          onIndex={setOpen}
          onClose={() => setOpen(null)}
          onChanged={(item) => mutate((p) => p && { ...p, items: p.items.map((x) => (x.id === item.id ? item : x)) })}
          onRemoved={(id) => mutate((p) => p && { ...p, items: p.items.filter((x) => x.id !== id) })}
        />
      </Show>
    </section>
  );
}
