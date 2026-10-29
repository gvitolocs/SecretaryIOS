import { For } from "solid-js";
import { t } from "../../../i18n";
import { COLORS, SIZES, TOOLS, type Size, type Tool } from "../../../lib/markupDraw";
import { ArrowIcon, HighlighterIcon, PenIcon, TextIcon, TrashIcon } from "./EditorIcons";
import type { EditorModel } from "./editorModel";

const ICONS = { pen: PenIcon, highlighter: HighlighterIcon, arrow: ArrowIcon, text: TextIcon } as const;
const TOOL_LABEL = { pen: "toolPen", highlighter: "toolHighlighter", arrow: "toolArrow", text: "toolText" } as const;
const SIZE_LABEL = { thin: "sizeThin", medium: "sizeMedium", thick: "sizeThick" } as const;

export interface MarkupSettings {
  tool: Tool;
  color: string;
  size: Size;
}

/** Tool, colour and size for the next mark; the marks themselves are drawn on the picture (MarkupOverlay). */
export function MarkupPanel(props: { model: EditorModel; settings: MarkupSettings; onChange: (next: MarkupSettings) => void }) {
  const set = (patch: Partial<MarkupSettings>) => props.onChange({ ...props.settings, ...patch });
  return (
    <div class="ed-panel-body" role="tabpanel" aria-label={t("editor.tabMarkup")}>
      <div class="ed-chips" role="radiogroup" aria-label={t("editor.tabMarkup")}>
        <For each={TOOLS}>
          {(tool) => {
            const Icon = ICONS[tool];
            return (
              <button type="button" role="radio" aria-checked={props.settings.tool === tool} class="ed-chip" classList={{ "ed-chip--on": props.settings.tool === tool }} onClick={() => set({ tool })}>
                <Icon />
                <span>{t(`editor.${TOOL_LABEL[tool]}`)}</span>
              </button>
            );
          }}
        </For>
      </div>
      <div class="ed-group-label" id="ed-color-label">
        {t("editor.color")}
      </div>
      <div class="ed-swatches" role="radiogroup" aria-labelledby="ed-color-label">
        <For each={COLORS}>
          {(color) => (
            <button
              type="button"
              role="radio"
              aria-checked={props.settings.color === color}
              aria-label={t("editor.colorOption", { color })}
              class="ed-swatch"
              classList={{ "ed-swatch--on": props.settings.color === color }}
              style={{ "--swatch": color }}
              onClick={() => set({ color })}
            />
          )}
        </For>
      </div>
      <div class="ed-group-label" id="ed-size-label">
        {t("editor.size")}
      </div>
      <div class="ed-chips" role="radiogroup" aria-labelledby="ed-size-label">
        <For each={SIZES}>
          {(size) => (
            <button type="button" role="radio" aria-checked={props.settings.size === size} class="ed-chip" classList={{ "ed-chip--on": props.settings.size === size }} onClick={() => set({ size })}>
              {t(`editor.${SIZE_LABEL[size]}`)}
            </button>
          )}
        </For>
      </div>
      <p class="ed-hint">{t("editor.markupHint")}</p>
      <div class="ed-row">
        <button type="button" class="ed-chip" disabled={props.model.list().markup.length === 0} onClick={() => props.model.commit({ ...props.model.committed(), markup: [] })}>
          <TrashIcon />
          <span>{t("editor.clearMarkup")}</span>
        </button>
      </div>
    </div>
  );
}
