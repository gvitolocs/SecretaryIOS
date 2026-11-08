import { createMemo, For, Show, type Accessor, type JSX } from "solid-js";
import type { ItemFace } from "../../../api/gallery";
import { t } from "../../../i18n";
import { boxToScreen, personName, placedFromStyle, type Rect } from "../../../lib/galleryPeople";
import "../../../styles/gallery-people.css";

export interface FaceLayerProps {
  faces: Accessor<ItemFace[]>;
  /** The picture's box as the viewer places it (PhotoViewer's `renderOverlay` gives it): fit, zoom, pan and rotation. */
  style: Accessor<JSX.CSSProperties | undefined>;
  placed: Accessor<boolean>;
  onPick: (face: ItemFace) => void;
}

const px = (n: number): string => `${Math.round(n * 10) / 10}px`;

/**
 * The face boxes over the current picture, each labelled with its person (or asking who it is). The layer fills the
 * stage and the boxes are placed in stage pixels from the picture's box, so they stay on the faces under zoom, pan and
 * rotation. A box is a button: clicking it opens the picker for that face.
 */
export function FaceLayer(props: FaceLayerProps) {
  const placed = createMemo(() => (props.placed() ? placedFromStyle(props.style() as Record<string, string | undefined> | undefined) : null));
  const boxes = createMemo(() => {
    const at = placed();
    if (!at) return [];
    return props.faces().map((face) => ({ face, rect: boxToScreen(face.box, at.rect, at.turn) }));
  });
  return (
    <div class="gf-layer" data-testid="face-layer">
      <For each={boxes()}>{(entry) => <FaceBoxButton face={entry.face} rect={entry.rect} onPick={props.onPick} />}</For>
    </div>
  );
}

function FaceBoxButton(props: { face: ItemFace; rect: Rect; onPick: (face: ItemFace) => void }) {
  const label = () => {
    const person = props.face.person;
    return person ? personName(person, t("galleryPeople.unnamed")) : t("galleryPeople.pickerTitle");
  };
  return (
    <button
      type="button"
      class="gf-box"
      classList={{ "gf-box--open": !props.face.person }}
      style={{ left: px(props.rect.x), top: px(props.rect.y), width: px(props.rect.w), height: px(props.rect.h) }}
      aria-label={props.face.person ? t("galleryPeople.faceOf", { name: label() }) : t("galleryPeople.faceUnassigned")}
      onClick={() => props.onPick(props.face)}
    >
      <Show when={label()}>
        <span class="gf-box__label">{label()}</span>
      </Show>
    </button>
  );
}
