import { createResource, createSignal, For, Show } from "solid-js";
import { listOnThisDay, listRecent, tileUrl, type GalleryItem } from "../../../api/gallery";
import { t } from "../../../i18n";
import { memoryCards, type MemoryCard } from "../../../lib/memories";
import { supportsJxl } from "../../../lib/jxl";
import { readString, writeString } from "../../../lib/storage";
import { GalleryViewer } from "../GalleryViewer";
import "../../../styles/gallery-browse.css";

const HIDDEN_KEY = "secretary.gallery.memoriesHidden";
/** "Recently added" looks back this many days. */
const RECENT_DAYS = 14;

export interface MemoriesStripProps {
  /** The Gallery's source (`all`, `device:<id>`, `chat`, `lucina`): memories come from the same pictures. */
  origin: string;
  /** Something changed in a memory's viewer (archived, deleted, favorited): the grid should reload. */
  onMutated?: () => void;
}

/**
 * The strip at the top of the Gallery: "On this day" cards (one per past year that has pictures on today's date) and
 * "Recently added". A card opens the viewer on its set; an edit in the viewer reaches the grid through `onMutated`.
 * It stays out of the way: nothing to show shows nothing, and the owner can fold it away.
 */
export function MemoriesStrip(props: MemoriesStripProps) {
  const [jxl] = createResource(supportsJxl);
  const [hidden, setHidden] = createSignal(readString("local", HIDDEN_KEY) === "1");
  const [cards] = createResource(
    () => props.origin,
    async (origin): Promise<MemoryCard[]> => {
      const [years, recent] = await Promise.all([
        listOnThisDay({ origin }).catch(() => []),
        listRecent({ origin, days: RECENT_DAYS, limit: 30 }).catch(() => ({ items: [] as GalleryItem[], total: 0 })),
      ]);
      return memoryCards(years, recent.items, recent.total);
    },
  );
  const [open, setOpen] = createSignal<{ id: string; items: GalleryItem[]; index: number } | null>(null);

  const toggle = () => {
    const next = !hidden();
    setHidden(next);
    writeString("local", HIDDEN_KEY, next ? "1" : "0");
  };

  const label = (card: MemoryCard) =>
    card.kind === "recent" ? t("browse.recentlyAdded") : t("browse.onThisDay");
  const sub = (card: MemoryCard) =>
    card.kind === "recent" ? t("browse.memoryCount", { count: card.total }) : t("browse.yearsAgo", { count: card.yearsAgo ?? 0 });
  const aria = (card: MemoryCard) =>
    card.kind === "recent" ? t("browse.recentOpen") : t("browse.memoryOpen", { year: String(card.year ?? "") });

  const changed = (item: GalleryItem) => {
    setOpen((o) => (o ? { ...o, items: o.items.map((i) => (i.id === item.id ? item : i)) } : o));
    props.onMutated?.();
  };
  const removed = (id: string) => {
    setOpen((o) => {
      if (!o) return o;
      const items = o.items.filter((i) => i.id !== id);
      return items.length ? { ...o, items, index: Math.min(o.index, items.length - 1) } : null;
    });
    props.onMutated?.();
  };

  return (
    <Show when={(cards()?.length ?? 0) > 0}>
      <section class="g-memories" aria-label={t("browse.memories")}>
        <div class="g-memories__head">
          <h2>{t("browse.memories")}</h2>
          <button type="button" class="g-chip g-chip--quiet" aria-expanded={!hidden()} onClick={toggle}>
            {hidden() ? t("browse.memoriesShow") : t("browse.memoriesHide")}
          </button>
        </div>
        <Show when={!hidden()}>
          <ul class="g-memories__row">
            <For each={cards()}>
              {(card) => (
                <li>
                  <button type="button" class="g-memory" aria-label={aria(card)} onClick={() => setOpen({ id: card.id, items: card.items, index: 0 })}>
                    <Show when={tileUrl(card.cover, jxl() ?? false)}>
                      {(src) => <img src={src()} alt="" loading="lazy" draggable={false} />}
                    </Show>
                    <span class="g-memory__text">
                      <strong>{label(card)}</strong>
                      <span>{sub(card)}</span>
                    </span>
                  </button>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </section>
      <Show when={open()}>
        {(set) => (
          <GalleryViewer
            items={set().items}
            index={set().index}
            onIndex={(index) => setOpen((o) => (o ? { ...o, index } : o))}
            onClose={() => setOpen(null)}
            onChanged={changed}
            onRemoved={removed}
          />
        )}
      </Show>
    </Show>
  );
}
