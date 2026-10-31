import { createSignal, For, onMount, Show } from "solid-js";
import { listGallery, type GalleryItem } from "../../../api/gallery";
import { t } from "../../../i18n";

const PAGE = 60;

/**
 * The photos of the Gallery as a grid to pick one reference from (newest first, more on request). The picture being
 * edited is left out. Pictures only: a reference is never a video.
 */
export function ReferencePicker(props: { exclude: string; onPick: (item: GalleryItem) => void; onBack: () => void }) {
  const [items, setItems] = createSignal<GalleryItem[]>([]);
  const [cursor, setCursor] = createSignal<string | null>(null);
  const [state, setState] = createSignal<"loading" | "ready" | "error">("loading");
  const [more, setMore] = createSignal(false);

  const load = async (from: string | null) => {
    setState(from ? "ready" : "loading");
    setMore(true);
    try {
      const page = await listGallery({ kind: "photo", limit: PAGE, ...(from ? { cursor: from } : {}) });
      setItems((all) => [...all, ...page.items.filter((i) => i.id !== props.exclude && !all.some((a) => a.id === i.id))]);
      setCursor(page.nextCursor);
      setState("ready");
    } catch {
      setState("error");
    } finally {
      setMore(false);
    }
  };

  onMount(() => void load(null));

  return (
    <div class="lab-picker">
      <div class="lab-picker-head">
        <button type="button" class="btn btn--small" onClick={() => props.onBack()}>
          {t("lab.referencePickerClose")}
        </button>
        <h3 class="lab-picker-title" id="lab-sheet-title">
          {t("lab.referencePickerTitle")}
        </h3>
      </div>
      <Show when={state() !== "loading"} fallback={<p class="gv-dialog-sub">{t("lab.referencePickerLoading")}</p>}>
        <Show when={state() !== "error" || items().length > 0} fallback={<p class="gv-error" role="alert">{t("lab.referencePickerError")}</p>}>
          <Show when={items().length > 0} fallback={<p class="gv-dialog-sub">{t("lab.referencePickerEmpty")}</p>}>
            <ul class="lab-picker-grid">
              <For each={items()}>
                {(item) => (
                  <li>
                    <button type="button" class="lab-picker-tile" title={item.title} aria-label={item.title || item.origin.filename || item.id} onClick={() => props.onPick(item)}>
                      <img src={item.urls.thumb ?? item.urls.large ?? ""} alt="" loading="lazy" draggable={false} />
                    </button>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </Show>
        <Show when={cursor()}>
          {(next) => (
            <button type="button" class="btn btn--small lab-picker-more" disabled={more()} onClick={() => void load(next())}>
              {t("lab.referencePickerMore")}
            </button>
          )}
        </Show>
      </Show>
    </div>
  );
}
