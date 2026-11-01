import { For } from "solid-js";
import { t } from "../../../i18n";
import { ASPECTS, aspectRatio, fitAspect, type AspectId } from "../../../lib/cropMath";
import { MAX_STRAIGHTEN, rotateEdit, setStraighten } from "../../../lib/editList";
import { ResetIcon, RotateLeftIcon, RotateRightIcon } from "./EditorIcons";
import type { EditorModel } from "./editorModel";
import { Slider } from "./Slider";

/** Aspect presets, quarter turns and the straighten dial. The crop box itself is dragged on the picture (CropOverlay). */
export function CropPanel(props: { model: EditorModel; aspect: AspectId; onAspect: (id: AspectId) => void; frameAspect: number }) {
  const { model } = props;
  const pick = (id: AspectId) => {
    props.onAspect(id);
    const ratio = aspectRatio(id, props.frameAspect);
    if (ratio !== null) model.commit({ ...model.committed(), crop: fitAspect(model.committed().crop, ratio, props.frameAspect) });
  };
  return (
    <div class="ed-panel-body" role="tabpanel" aria-label={t("editor.tabCrop")}>
      <div class="ed-row">
        <button type="button" class="ed-chip" onClick={() => model.commit(rotateEdit(model.committed(), -1))}>
          <RotateLeftIcon />
          <span>{t("editor.rotateLeft")}</span>
        </button>
        <button type="button" class="ed-chip" onClick={() => model.commit(rotateEdit(model.committed(), 1))}>
          <RotateRightIcon />
          <span>{t("editor.rotateRight")}</span>
        </button>
        <button
          type="button"
          class="ed-chip"
          disabled={model.list().crop === null}
          onClick={() => {
            props.onAspect("free");
            model.commit({ ...model.committed(), crop: null });
          }}
        >
          <ResetIcon />
          <span>{t("editor.resetCrop")}</span>
        </button>
      </div>
      <Slider
        id="ed-straighten"
        label={t("editor.straighten")}
        value={model.list().straighten}
        min={-MAX_STRAIGHTEN}
        max={MAX_STRAIGHTEN}
        step={0.1}
        format={(v) => t("editor.straightenValue", { value: v.toFixed(1) })}
        onPreview={(v) => model.preview(setStraighten(model.committed(), v))}
        onCommit={(v) => model.commit(setStraighten(model.committed(), v))}
        onReset={() => model.commit(setStraighten(model.committed(), 0))}
      />
      <div class="ed-group-label" id="ed-aspect-label">
        {t("editor.aspect")}
      </div>
      <div class="ed-chips" role="radiogroup" aria-labelledby="ed-aspect-label">
        <For each={ASPECTS}>
          {(a) => (
            <button type="button" role="radio" aria-checked={props.aspect === a.id} class="ed-chip" classList={{ "ed-chip--on": props.aspect === a.id }} onClick={() => pick(a.id)}>
              {t(`editor.aspect${a.id}`)}
            </button>
          )}
        </For>
      </div>
    </div>
  );
}
