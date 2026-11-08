import { For, createSignal } from "solid-js";
import { t } from "../../../i18n";
import { FULL, hitHandle, resizeCrop, type Handle } from "../../../lib/cropMath";
import type { Crop } from "../../../lib/editList";
import type { EditorModel } from "./editorModel";

const HANDLES: Handle[] = ["nw", "n", "ne", "e", "se", "s", "sw", "w"];
/** How near (in pixels) a press must be to a side or corner to grab it. */
const REACH = 24;

/**
 * The crop box over the straightened frame: drag its sides and corners (or the inside to move it). `aspect` is the
 * locked pixel ratio or null. While dragging the box previews; letting go commits one undo step.
 */
export function CropOverlay(props: { model: EditorModel; aspect: number | null; frameAspect: number }) {
  const { model } = props;
  const box = (): Crop => model.list().crop ?? FULL;
  const [dragging, setDragging] = createSignal(false);
  let el!: HTMLDivElement;
  let drag: { handle: Handle; x: number; y: number; start: Crop; w: number; h: number } | null = null;

  const press = (e: PointerEvent) => {
    const rect = el.getBoundingClientRect();
    const c = box();
    const handle = hitHandle(
      { x: c.x * rect.width, y: c.y * rect.height, w: c.w * rect.width, h: c.h * rect.height },
      e.clientX - rect.left,
      e.clientY - rect.top,
      REACH,
    );
    if (!handle) return;
    e.preventDefault();
    el.setPointerCapture(e.pointerId);
    drag = { handle, x: e.clientX, y: e.clientY, start: c, w: rect.width, h: rect.height };
    setDragging(true);
  };

  const move = (e: PointerEvent) => {
    if (!drag) return;
    const next = resizeCrop(drag.start, drag.handle, (e.clientX - drag.x) / drag.w, (e.clientY - drag.y) / drag.h, props.aspect, props.frameAspect);
    model.preview({ ...model.committed(), crop: next });
  };

  const release = (e: PointerEvent) => {
    if (!drag) return;
    drag = null;
    setDragging(false);
    if (el.hasPointerCapture(e.pointerId)) el.releasePointerCapture(e.pointerId);
    const c = model.list().crop;
    // A box that is the whole frame again is no crop.
    model.commit({ ...model.committed(), crop: c && c.x === 0 && c.y === 0 && c.w === 1 && c.h === 1 ? null : c });
  };

  return (
    <div
      ref={el}
      class="ed-crop"
      classList={{ "ed-crop--dragging": dragging() }}
      onPointerDown={press}
      onPointerMove={move}
      onPointerUp={release}
      onPointerCancel={release}
    >
      <div
        class="ed-crop-box"
        role="group"
        aria-label={t("editor.cropBox")}
        style={{ left: `${box().x * 100}%`, top: `${box().y * 100}%`, width: `${box().w * 100}%`, height: `${box().h * 100}%` }}
      >
        <span class="ed-crop-grid" />
        <For each={HANDLES}>{(h) => <span class={`ed-handle ed-handle--${h}`} />}</For>
      </div>
    </div>
  );
}
