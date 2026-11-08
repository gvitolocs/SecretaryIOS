import { For } from "solid-js";
import { t } from "../../../i18n";
import { ADJUST_CONTROLS, neutralAdjust, setAdjust, type AdjustKey } from "../../../lib/editList";
import { AutoIcon, ResetIcon } from "./EditorIcons";
import type { EditorModel } from "./editorModel";
import { Slider } from "./Slider";

const format = (key: AdjustKey) => (v: number) => (key === "exposure" ? (v > 0 ? "+" : "") + v.toFixed(2) : v > 0 && key !== "fade" ? `+${Math.round(v)}` : String(Math.round(v)));

/** Exposure, contrast, highlights, shadows, saturation, warmth, tint, vignette and fade, plus Auto. */
export function AdjustPanel(props: { model: EditorModel; onAuto: () => void; autoDisabled?: boolean }) {
  const { model } = props;
  const adjustChanged = () => Object.values(model.list().adjust).some((v) => v !== 0);
  return (
    <div class="ed-panel-body" role="tabpanel" aria-label={t("editor.tabAdjust")}>
      <div class="ed-row">
        <button type="button" class="ed-chip" title={t("editor.autoHint")} disabled={props.autoDisabled} onClick={props.onAuto}>
          <AutoIcon />
          <span>{t("editor.auto")}</span>
        </button>
        <button
          type="button"
          class="ed-chip"
          disabled={!adjustChanged()}
          onClick={() => model.commit({ ...model.committed(), adjust: neutralAdjust() })}
        >
          <ResetIcon />
          <span>{t("editor.resetAll")}</span>
        </button>
      </div>
      <For each={ADJUST_CONTROLS}>
        {(c) => (
          <Slider
            id={`ed-adjust-${c.key}`}
            label={t(`editor.${c.key}`)}
            value={model.list().adjust[c.key]}
            min={c.min}
            max={c.max}
            step={c.step}
            format={format(c.key)}
            onPreview={(v) => model.preview(setAdjust(model.committed(), c.key, v))}
            onCommit={(v) => model.commit(setAdjust(model.committed(), c.key, v))}
            onReset={() => model.commit(setAdjust(model.committed(), c.key, 0))}
          />
        )}
      </For>
    </div>
  );
}
