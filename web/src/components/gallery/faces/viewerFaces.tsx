import { createEffect, createResource, createSignal, on, Show, type Accessor, type JSX } from "solid-js";
import { assignFace, listItemFaces, type FaceTarget, type ItemFace } from "../../../api/gallery";
import { t } from "../../../i18n";
import { readString, writeString } from "../../../lib/storage";
import { FacesIcon } from "./FaceIcons";
import { FaceLayer } from "./FaceLayer";
import { FacePicker } from "./FacePicker";
import { facesAvailable } from "./facesAvailable";

const FACES_KEY = "gallery.facesOn";

export interface ViewerFacesHooks {
  /** A face was assigned or unassigned: the photo's people changed. */
  onPeopleChanged?: () => void;
  /** A message for the viewer's toast. */
  onError?: (message: string) => void;
}

export interface OverlayContext {
  current: Accessor<boolean>;
  style: Accessor<JSX.CSSProperties | undefined>;
  placed: Accessor<boolean>;
}

/**
 * Everything the photo viewer needs for faces, so the viewer itself only places four things: the toolbar `button()`, the
 * overlay `layer()`, the `picker()` dialog and the P key (`toggle()`). Faces are asked for only when recognition is on
 * and the toggle is on, for the current photo.
 */
export function createViewerFaces(itemId: Accessor<string | undefined>, hooks: ViewerFacesHooks = {}) {
  const [available] = createResource(facesAvailable);
  const [wanted, setWanted] = createSignal(readString("local", FACES_KEY) === "1");
  const [revision, setRevision] = createSignal(0);
  const [picking, setPicking] = createSignal<ItemFace | null>(null);
  const isAvailable = () => available() === true;
  const shown = () => isAvailable() && wanted();

  const [data, { mutate, refetch }] = createResource(
    () => (shown() ? (itemId() ?? false) : false),
    async (id) => ({ id, faces: await listItemFaces(id) }),
  );
  const faces = (): ItemFace[] => {
    if (data.error) return [];
    const loaded = data();
    return loaded && loaded.id === itemId() ? loaded.faces : [];
  };

  createEffect(
    on(
      () => data.error,
      (failed) => {
        if (failed) hooks.onError?.(t("galleryPeople.facesFailed"));
      },
      { defer: true },
    ),
  );
  // Another photo: a picker for the old one is closed.
  createEffect(on(itemId, () => setPicking(null), { defer: true }));

  function toggle(): void {
    const next = !wanted();
    setWanted(next);
    writeString("local", FACES_KEY, next ? "1" : "0");
  }

  async function assign(face: ItemFace, target: FaceTarget): Promise<void> {
    const id = itemId();
    const updated = await assignFace(face.id, target);
    if (updated && id) mutate((cur) => (cur && cur.id === id ? { id, faces: cur.faces.map((f) => (f.id === updated.id ? updated : f)) } : cur));
    else await refetch();
    setPicking(null);
    setRevision((r) => r + 1);
    hooks.onPeopleChanged?.();
  }

  return {
    /** Recognition is on: the toggle and the key work. */
    available: isAvailable,
    on: shown,
    toggle,
    /** Bumped after every assignment, so the info panel asks for the photo's people again. */
    revision,
    /** The picker is open: the viewer's own keys step aside. */
    picking: () => picking() !== null,
    button(): JSX.Element {
      return (
        <Show when={isAvailable()}>
          <button
            type="button"
            class="pv-btn"
            classList={{ "pv-btn--active": wanted() }}
            aria-label={t("galleryPeople.faces")}
            aria-pressed={wanted()}
            title={`${wanted() ? t("galleryPeople.facesHide") : t("galleryPeople.facesToggle")} (P)`}
            onClick={toggle}
          >
            <FacesIcon />
          </button>
        </Show>
      );
    },
    layer(ctx: OverlayContext): JSX.Element {
      return (
        <Show when={shown() && ctx.current()}>
          <FaceLayer faces={faces} style={ctx.style} placed={ctx.placed} onPick={setPicking} />
        </Show>
      );
    },
    picker(): JSX.Element {
      return (
        <Show when={picking()}>
          {(face) => <FacePicker faceId={face().id} current={face().person} onAssign={(target) => assign(face(), target)} onCancel={() => setPicking(null)} />}
        </Show>
      );
    },
  };
}
