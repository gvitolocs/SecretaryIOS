import { createEffect, createSignal, For, on, Show } from "solid-js";
import { assignFace, listUnassignedFaces, type FaceTarget, type UnassignedFace } from "../../../api/gallery";
import { Card } from "../../page";
import { FacePicker } from "../faces/FacePicker";
import "../../../styles/gallery-people.css";
import { groupText, useGroupKind } from "../../../lib/galleryGroup";

/**
 * "Faces without a person" at the bottom of the People page: each face can be given to a person (or to a new one) with the
 * same picker as in the viewer. The list is asked for again when `refreshKey` changes (the scan found more).
 */
export function UnassignedFaces(props: { wide?: boolean; refreshKey: number; onAssigned: () => void }) {
  const kind = useGroupKind();
  const pt = groupText(kind);
  const [faces, setFaces] = createSignal<UnassignedFace[]>([]);
  const [cursor, setCursor] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  const [choosing, setChoosing] = createSignal<UnassignedFace | null>(null);
  let generation = 0;

  async function load(more: boolean): Promise<void> {
    const mine = more ? generation : ++generation;
    setLoading(true);
    setFailed(false);
    try {
      const page = await listUnassignedFaces({ cursor: more ? cursor() : null }, kind);
      if (mine !== generation) return;
      setFaces((prev) => {
        const have = new Set(more ? prev.map((f) => f.id) : []);
        return (more ? prev : []).concat(page.faces.filter((f) => !have.has(f.id)));
      });
      setCursor(page.nextCursor);
    } catch {
      if (mine === generation) setFailed(true);
    } finally {
      if (mine === generation) setLoading(false);
    }
  }

  createEffect(on(() => props.refreshKey, () => void load(false)));

  async function assign(face: UnassignedFace, target: FaceTarget): Promise<void> {
    await assignFace(face.id, target, kind);
    setChoosing(null);
    setFaces((prev) => prev.filter((f) => f.id !== face.id));
    props.onAssigned();
  }

  const content = () => (
    <>
        <p class="pp-hint">{pt("unassignedHint")}</p>
        <ul class="pp-faces">
          <For each={faces()}>
            {(face) => (
              <li>
                <button type="button" class="pp-faces__btn" aria-label={pt("unassignedFace")} onClick={() => setChoosing(face)}>
                  <img src={face.thumbUrl} alt="" loading="lazy" draggable={false} />
                </button>
              </li>
            )}
          </For>
        </ul>
        <Show when={failed()}>
          <p class="gv-error" role="alert">
            {pt("loadError")}{" "}
            <button type="button" class={props.wide ? "pg-btn" : "btn"} onClick={() => void load(cursor() !== null && faces().length > 0)}>
              {pt("retry")}
            </button>
          </p>
        </Show>
        <Show when={cursor() && !failed()}>
          <button type="button" class={props.wide ? "pg-btn" : "btn"} disabled={loading()} onClick={() => void load(true)}>
            {pt("showMore")}
          </button>
        </Show>
    </>
  );

  return (
    <Show when={faces().length > 0 || failed()}>
      <Show
        when={props.wide}
        fallback={
          <section class="pp-unassigned" aria-labelledby="pp-unassigned-title">
            <h2 id="pp-unassigned-title" class="pp-section">
              {pt("unassignedTitle")}
            </h2>
            {content()}
          </section>
        }
      >
        <Card title={pt("unassignedTitle")} class="pp-unassigned">
          {content()}
        </Card>
      </Show>
      <Show when={choosing()}>
        {(face) => <FacePicker faceId={face().id} current={null} onAssign={(target) => assign(face(), target)} onCancel={() => setChoosing(null)} />}
      </Show>
    </Show>
  );
}
