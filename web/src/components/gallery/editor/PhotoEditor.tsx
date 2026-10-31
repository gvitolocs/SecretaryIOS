import { createEffect, createMemo, createResource, createSignal, For, Match, onCleanup, onMount, Show, Switch, untrack } from "solid-js";
import { Portal } from "solid-js/web";
import { ApiError } from "../../../api/client";
import { deleteEdits, getEdits, patchItem, putEdits, type GalleryItem, type ItemEdits } from "../../../api/gallery";
import { t } from "../../../i18n";
import { aspectRatio, fitBox, type AspectId } from "../../../lib/cropMath";
import { emptyEdit, FILTER_NAMES, frameSize, quartersOf, rotateEdit } from "../../../lib/editList";
import { imageCore, type EditSource, type ImageCore, type Pixels } from "../../../lib/imageCore";
import { COLORS } from "../../../lib/markupDraw";
import { Modal } from "../Modal";
import "../../../styles/gallery-editor.css";
import { AdjustPanel } from "./AdjustPanel";
import { CropOverlay } from "./CropOverlay";
import { CropPanel } from "./CropPanel";
import { CompareIcon, CropIcon, EditIcon, FilterIcon, MarkupIcon, RedoIcon, UndoIcon } from "./EditorIcons";
import { createEditorModel } from "./editorModel";
import { FiltersPanel, type FilterTiles } from "./FiltersPanel";
import { loadSource, type LoadedSource } from "./loadSource";
import { MarkupOverlay } from "./MarkupOverlay";
import { MarkupPanel, type MarkupSettings } from "./MarkupPanel";

export interface PhotoEditorProps {
  item: GalleryItem;
  /** Leave without saving. */
  onClose: () => void;
  /** A save or a revert went through: the item as the server has it now. The parent closes the editor. */
  onSaved: (item: GalleryItem) => void;
}

type Tab = "adjust" | "crop" | "filters" | "markup";
type Ask = "discard" | "revert" | null;

interface Loaded {
  state: ItemEdits;
  source: LoadedSource;
}

async function load(item: GalleryItem): Promise<Loaded> {
  const state = await getEdits(item.id);
  const url = state.baseUrl ?? item.urls.large ?? item.urls.preview;
  if (!url) throw new Error("no picture to edit");
  return { state, source: await loadSource(url) };
}

/**
 * The photo editor, full screen over the viewer: Adjust, Crop & rotate, Filters and Markup on a live preview that the
 * Rust core renders (image-core.wasm, on a copy of at most 1600 px), with undo and redo, hold-to-compare, Save (the
 * server renders the full-size derivative) and, for an edited photo, "Revert to original". The original file is never
 * changed.
 */
export function PhotoEditor(props: PhotoEditorProps) {
  const [core] = createResource(() => imageCore());
  // Loaded once per photo: a save that updates `props.item` must not reload the picture under the editor.
  const [loaded] = createResource(() => props.item.id, () => load(untrack(() => props.item)));

  return (
    <Portal>
      <div class="ed" role="dialog" aria-modal="true" aria-label={t("editor.title")}>
        <Switch>
          <Match when={loaded.error}>
            <Notice message={t("editor.loadFailed")} onClose={props.onClose} />
          </Match>
          <Match when={core.state === "ready" && core() === null}>
            <Notice message={t("editor.noCore")} onClose={props.onClose} />
          </Match>
          <Match when={core() && loaded()}>
            <EditorBody item={props.item} state={loaded()!.state} source={loaded()!.source} core={core()!} onClose={props.onClose} onSaved={props.onSaved} />
          </Match>
          <Match when={true}>
            <div class="ed-loading" role="status">
              {t("editor.loading")}
            </div>
          </Match>
        </Switch>
      </div>
    </Portal>
  );
}

function Notice(props: { message: string; onClose: () => void }) {
  return (
    <div class="ed-loading" role="alert">
      <p>{props.message}</p>
      <button type="button" class="btn" onClick={props.onClose}>
        {t("editor.cancel")}
      </button>
    </div>
  );
}

function EditorBody(props: { item: GalleryItem; state: ItemEdits; source: LoadedSource; core: ImageCore; onClose: () => void; onSaved: (item: GalleryItem) => void }) {
  const full = props.source.full;
  const stored = props.state.edits ?? emptyEdit();
  const model = createEditorModel(stored);

  // The viewer's rotation (R) is drawn by CSS on top of the picture; the edit bakes it in, and the save resets it. With
  // markup already stored the quarter turns would have to move the marks too, so the rotation is left alone then.
  const turns = quartersOf(props.item.rotation);
  const bakeRotation = turns > 0 && stored.markup.length === 0;
  if (bakeRotation) {
    let turned = model.committed();
    for (let i = 0; i < turns; i++) turned = rotateEdit(turned, 1);
    model.commit(turned);
  }

  const source: EditSource = props.core.editSource(full.rgba, full.width, full.height);
  onCleanup(() => source.free());
  // While a slider or the crop box is dragged the smaller copy is rendered (when there is one), the full one once it is let go.
  const draftSource: EditSource | null = props.source.draft === full ? null : props.core.editSource(props.source.draft.rgba, props.source.draft.width, props.source.draft.height);
  onCleanup(() => draftSource?.free());

  const [tab, setTab] = createSignal<Tab>("adjust");
  const [aspect, setAspect] = createSignal<AspectId>("free");
  const [markup, setMarkup] = createSignal<MarkupSettings>({ tool: "pen", color: COLORS[2]!, size: "medium" });
  const [comparing, setComparing] = createSignal(false);
  const [ask, setAsk] = createSignal<Ask>(null);
  const [busy, setBusy] = createSignal<"save" | "revert" | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [tiles, setTiles] = createSignal<FilterTiles>({});

  // ---- the preview --------------------------------------------------------------------------------------------------

  const [shownSize, setShownSize] = createSignal({ width: 1, height: 1 });
  const [stageBox, setStageBox] = createSignal({ width: 0, height: 0 });
  const [renderMs, setRenderMs] = createSignal(0);
  const [renderFailed, setRenderFailed] = createSignal(false);
  let stage: HTMLDivElement | undefined;
  let canvas: HTMLCanvasElement | undefined;

  const frame = createMemo(() => frameSize(full.width, full.height, model.list().rotate, model.list().straighten));
  const frameAspect = () => frame().width / frame().height;

  /** What the canvas shows: the original while comparing, the whole straightened frame on the Crop tab, else the result. */
  const viewJson = createMemo(() => {
    if (comparing()) return JSON.stringify(emptyEdit());
    const list = model.list();
    return JSON.stringify(tab() === "crop" ? { ...list, crop: null } : list);
  });

  function draw(pixels: Pixels) {
    if (!canvas) return;
    if (canvas.width !== pixels.width || canvas.height !== pixels.height) {
      canvas.width = pixels.width;
      canvas.height = pixels.height;
    }
    canvas.getContext("2d")?.putImageData(new ImageData(pixels.rgba, pixels.width, pixels.height), 0, 0);
    setShownSize({ width: pixels.width, height: pixels.height });
  }

  // Renders are coalesced to one per frame: a slider that moves faster than the core renders skips the in-between lists.
  let pending: { json: string; draft: boolean } | null = null;
  let raf = 0;
  function flush() {
    raf = 0;
    const job = pending;
    pending = null;
    if (job === null) return;
    try {
      const started = performance.now();
      draw((job.draft && draftSource ? draftSource : source).render(job.json));
      setRenderMs(Math.round((performance.now() - started) * 10) / 10);
      setRenderFailed(false);
    } catch {
      setRenderFailed(true);
    }
  }
  createEffect(() => {
    pending = { json: viewJson(), draft: model.dragging() };
    if (!raf) raf = requestAnimationFrame(flush);
  });
  onCleanup(() => cancelAnimationFrame(raf));

  onMount(() => {
    if (!stage || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry) setStageBox({ width: entry.contentRect.width, height: entry.contentRect.height });
    });
    observer.observe(stage);
    onCleanup(() => observer.disconnect());
  });
  const fitted = createMemo(() => fitBox(stageBox().width, stageBox().height, shownSize().width, shownSize().height));

  // The filter tiles: each filter on a 160 px copy, made once.
  onMount(() => {
    const small = props.source.tile;
    let tileSource: EditSource | undefined;
    try {
      tileSource = props.core.editSource(small.rgba, small.width, small.height);
      const made: FilterTiles = {};
      for (const name of [null, ...FILTER_NAMES]) {
        made[name ?? "none"] = tileSource.render({ ...emptyEdit(), filter: name ? { name, strength: 1 } : null });
      }
      setTiles(made);
    } catch {
      // The tiles are a nicety; the filters still work without their previews.
    } finally {
      tileSource?.free();
    }
  });

  // ---- actions ------------------------------------------------------------------------------------------------------

  function auto() {
    try {
      model.commit({ ...model.committed(), adjust: source.auto() });
    } catch {
      setError(t("editor.renderFailed"));
    }
  }

  function requestClose() {
    if (busy()) return;
    if (model.dirty()) setAsk("discard");
    else props.onClose();
  }

  async function save() {
    if (busy()) return;
    setBusy("save");
    setError(null);
    try {
      const result = await putEdits(props.item.id, model.result());
      let item = result.item ?? props.item;
      if (bakeRotation) {
        // The turn is in the picture now; drawing it again would turn it twice.
        item = (await patchItem(item.id, { rotation: 0 }).catch(() => null)) ?? { ...item, rotation: 0 };
      }
      props.onSaved(item);
    } catch (err) {
      setError(err instanceof ApiError && err.status === 422 ? t("editor.saveRefused") : t("editor.saveFailed"));
      setBusy(null);
    }
  }

  async function revert() {
    setBusy("revert");
    setError(null);
    try {
      const result = await deleteEdits(props.item.id);
      props.onSaved(result.item ?? { ...props.item, edited: false });
    } catch {
      setError(t("editor.revertFailed"));
      setBusy(null);
      setAsk(null);
    }
  }

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      if (ask()) return;
      e.preventDefault();
      requestClose();
      return;
    }
    const inField = e.target instanceof Element && !!e.target.closest("input[type='text'], textarea");
    if (!(e.metaKey || e.ctrlKey) || e.altKey || inField || ask()) return;
    const key = e.key.toLowerCase();
    if (key === "z") {
      e.preventDefault();
      if (e.shiftKey) model.redo();
      else model.undo();
    } else if (key === "y") {
      e.preventDefault();
      model.redo();
    }
  };
  onMount(() => {
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  const tabs: { id: Tab; label: () => string; icon: () => import("solid-js").JSX.Element }[] = [
    { id: "adjust", label: () => t("editor.tabAdjust"), icon: () => <EditIcon /> },
    { id: "crop", label: () => t("editor.tabCrop"), icon: () => <CropIcon /> },
    { id: "filters", label: () => t("editor.tabFilters"), icon: () => <FilterIcon /> },
    { id: "markup", label: () => t("editor.tabMarkup"), icon: () => <MarkupIcon /> },
  ];

  return (
    <>
      <header class="ed-top">
        <button type="button" class="ed-btn" disabled={busy() !== null} onClick={requestClose}>
          {t("editor.cancel")}
        </button>
        <h2 class="ed-title">{t("editor.title")}</h2>
        <div class="ed-top-actions">
          <button type="button" class="ed-icon-btn" aria-label={t("editor.undo")} title={`${t("editor.undo")} (Ctrl+Z)`} disabled={!model.canUndo() || busy() !== null} onClick={() => model.undo()}>
            <UndoIcon />
          </button>
          <button type="button" class="ed-icon-btn" aria-label={t("editor.redo")} title={`${t("editor.redo")} (Ctrl+Shift+Z)`} disabled={!model.canRedo() || busy() !== null} onClick={() => model.redo()}>
            <RedoIcon />
          </button>
          <button
            type="button"
            class="ed-icon-btn"
            classList={{ "ed-icon-btn--on": comparing() }}
            aria-label={t("editor.compare")}
            title={t("editor.compare")}
            onPointerDown={(e) => {
              e.preventDefault();
              setComparing(true);
            }}
            onPointerUp={() => setComparing(false)}
            onPointerLeave={() => setComparing(false)}
            onPointerCancel={() => setComparing(false)}
            onKeyDown={(e) => {
              if (e.key === " " || e.key === "Enter") {
                e.preventDefault();
                setComparing(true);
              }
            }}
            onKeyUp={() => setComparing(false)}
            onBlur={() => setComparing(false)}
          >
            <CompareIcon />
          </button>
          <button type="button" class="ed-btn ed-btn--primary" disabled={busy() !== null || !model.dirty()} onClick={() => void save()}>
            {busy() === "save" ? t("editor.saving") : t("editor.save")}
          </button>
        </div>
      </header>

      <div class="ed-main">
        <div ref={stage} class="ed-stage" data-render-ms={renderMs()} data-tab={tab()}>
          <div class="ed-canvas-box" style={{ width: `${fitted().width}px`, height: `${fitted().height}px` }}>
            <canvas ref={canvas} class="ed-canvas" role="img" aria-label={t("editor.stage")} />
            <Show when={comparing()}>
              <span class="ed-badge">{t("editor.compareShort")}</span>
            </Show>
            <Show when={!comparing() && tab() === "crop"}>
              <CropOverlay model={model} aspect={aspectRatio(aspect(), frameAspect())} frameAspect={frameAspect()} />
            </Show>
            <Show when={!comparing() && tab() === "markup"}>
              <MarkupOverlay model={model} settings={markup()} width={fitted().width} cropW={model.list().crop?.w ?? 1} />
            </Show>
          </div>
          <Show when={renderFailed()}>
            <p class="ed-stage-error" role="alert">
              {t("editor.renderFailed")}
            </p>
          </Show>
        </div>

        <aside class="ed-panel">
          <div class="ed-tabs" role="tablist" aria-label={t("editor.tabs")}>
            <For each={tabs}>
              {(entry) => (
                <button type="button" role="tab" aria-selected={tab() === entry.id} class="ed-tab" classList={{ "ed-tab--on": tab() === entry.id }} onClick={() => setTab(entry.id)}>
                  {entry.icon()}
                  <span>{entry.label()}</span>
                </button>
              )}
            </For>
          </div>
          <Switch>
            <Match when={tab() === "adjust"}>
              <AdjustPanel model={model} onAuto={auto} />
            </Match>
            <Match when={tab() === "crop"}>
              <CropPanel model={model} aspect={aspect()} onAspect={setAspect} frameAspect={frameAspect()} />
            </Match>
            <Match when={tab() === "filters"}>
              <FiltersPanel model={model} tiles={tiles()} />
            </Match>
            <Match when={tab() === "markup"}>
              <MarkupPanel model={model} settings={markup()} onChange={setMarkup} />
            </Match>
          </Switch>
          <Show when={error()}>
            {(message) => (
              <p class="gv-error" role="alert">
                {message()}
              </p>
            )}
          </Show>
          <Show when={props.item.edited}>
            <button type="button" class="ed-revert" disabled={busy() !== null} onClick={() => setAsk("revert")}>
              {t("editor.revert")}
            </button>
          </Show>
        </aside>
      </div>

      <Switch>
        <Match when={ask() === "discard"}>
          <Modal labelledBy="ed-discard-title" describedBy="ed-discard-body" onClose={() => setAsk(null)}>
            <h2 id="ed-discard-title" class="gv-dialog-title">
              {t("editor.discardTitle")}
            </h2>
            <div id="ed-discard-body" class="gv-dialog-body">
              <p>{t("editor.discardLine")}</p>
            </div>
            <div class="gv-dialog-actions">
              <button type="button" class="btn" onClick={() => setAsk(null)}>
                {t("editor.keepEditing")}
              </button>
              <button type="button" class="btn btn--danger" onClick={() => props.onClose()}>
                {t("editor.discard")}
              </button>
            </div>
          </Modal>
        </Match>
        <Match when={ask() === "revert"}>
          <Modal labelledBy="ed-revert-title" describedBy="ed-revert-body" busy={busy() === "revert"} onClose={() => setAsk(null)}>
            <h2 id="ed-revert-title" class="gv-dialog-title">
              {t("editor.revertTitle")}
            </h2>
            <div id="ed-revert-body" class="gv-dialog-body">
              <p>{t("editor.revertLine")}</p>
            </div>
            <div class="gv-dialog-actions">
              <button type="button" class="btn" disabled={busy() === "revert"} onClick={() => setAsk(null)}>
                {t("editor.cancel")}
              </button>
              <button type="button" class="btn btn--danger" disabled={busy() === "revert"} onClick={() => void revert()}>
                {busy() === "revert" ? t("editor.reverting") : t("editor.revertConfirm")}
              </button>
            </div>
          </Modal>
        </Match>
      </Switch>
    </>
  );
}
