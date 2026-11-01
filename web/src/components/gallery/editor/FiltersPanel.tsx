import { For, Show, createEffect } from "solid-js";
import { t } from "../../../i18n";
import { FILTER_NAMES, setFilter, type FilterName } from "../../../lib/editList";
import type { Pixels } from "../../../lib/imageCore";
import type { EditorModel } from "./editorModel";
import { Slider } from "./Slider";

/** Previews by filter name; "none" is the unfiltered picture. Missing while they are being made. */
export type FilterTiles = Partial<Record<FilterName | "none", Pixels>>;

const ENTRIES: (FilterName | null)[] = [null, ...FILTER_NAMES];

function TileCanvas(props: { pixels: Pixels }) {
  let canvas!: HTMLCanvasElement;
  createEffect(() => {
    const { width, height, rgba } = props.pixels;
    canvas.width = width;
    canvas.height = height;
    canvas.getContext("2d")?.putImageData(new ImageData(rgba, width, height), 0, 0);
  });
  return <canvas ref={canvas} aria-hidden="true" />;
}

/** The named filters as small previews of this picture; a click sets the filter, the slider sets how much. */
export function FiltersPanel(props: { model: EditorModel; tiles: FilterTiles }) {
  const { model } = props;
  const current = () => model.list().filter;
  const label = (name: FilterName | null) => (name === null ? t("editor.filterNone") : t(`editor.filter${name}`));
  return (
    <div class="ed-panel-body" role="tabpanel" aria-label={t("editor.tabFilters")}>
      <div class="ed-tiles" role="radiogroup" aria-label={t("editor.tabFilters")}>
        <For each={ENTRIES}>
          {(name) => (
            <button
              type="button"
              role="radio"
              aria-checked={(current()?.name ?? null) === name}
              class="ed-tile"
              classList={{ "ed-tile--on": (current()?.name ?? null) === name }}
              onClick={() => model.commit(setFilter(model.committed(), name, model.committed().filter?.strength ?? 1))}
            >
              <span class="ed-tile-art">
                <Show when={props.tiles[name ?? "none"]}>{(pixels) => <TileCanvas pixels={pixels()} />}</Show>
              </span>
              <span class="ed-tile-name">{label(name)}</span>
            </button>
          )}
        </For>
      </div>
      <Show when={current()}>
        {(filter) => (
          <Slider
            id="ed-filter-strength"
            label={t("editor.strength")}
            value={Math.round(filter().strength * 100)}
            min={0}
            max={100}
            step={1}
            neutral={100}
            format={(v) => `${Math.round(v)}%`}
            onPreview={(v) => model.preview({ ...model.committed(), filter: { name: filter().name, strength: v / 100 } })}
            onCommit={(v) => model.commit({ ...model.committed(), filter: { name: filter().name, strength: v / 100 } })}
            onReset={() => model.commit({ ...model.committed(), filter: { name: filter().name, strength: 1 } })}
          />
        )}
      </Show>
    </div>
  );
}
