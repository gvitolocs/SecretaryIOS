import type { AccessoryEntry, AccessorySlot, AvatarEditor, Category, Look, MakeupLayer, PartEntry, PartSlot, SliderDef, WardrobeStage } from "@secretary/lucina-stage";
import { createMemo, createSignal, For, Match, onCleanup, onMount, Show, Switch, type JSX } from "solid-js";
import { locale, t, type MessageKey } from "../i18n";
import { loadStage, type StageModule } from "../lib/stageLoader";
import { ASSET_BASE, hairChoices, outfitName, thumbnailUrl, type SavedLook } from "../lib/wardrobe";
import { effectiveTheme, stageQuality } from "../state/appearance";
import {
  avatarSaving,
  avatarSettings,
  closeEditor,
  currentHair,
  currentHairColor,
  currentLook,
  currentOutfit,
  deleteLook,
  saveLook,
  wardrobeAccessories,
  wardrobeColors,
  wardrobeHairstyles,
  wardrobeOutfits,
  wardrobeParts,
  wearLook,
} from "../state/wardrobe";
import { ColorWheel } from "./ColorWheel";
import {
  BodyIcon,
  CheckIcon,
  CloseIcon,
  DiceIcon,
  DropIcon,
  EyeIcon,
  FaceIcon,
  GemIcon,
  HairIcon,
  HangerIcon,
  LipsIcon,
  RedoIcon,
  RefreshIcon,
  SparkleIcon,
  TrashIcon,
  UndoIcon,
} from "./Icons";
import { CLIPS_META_URL, CLIPS_URL, MANIFEST_URL, MODEL_URL } from "./LucinaPanel";

// "Edit Lucina", after Custom Cast's SceneEdit (docs/design/custom-cast-editor.md): a category
// rail, the live stage (the camera frames what is being edited), and a panel with a part grid,
// sliders or a colour wheel. Undo/redo, randomise, reset; looks are saved on the server
// (GET/PUT /v1/avatar `looks`, `current`). Our own Nebula/Pearl look; no assets of that app.

const AUTH: RequestInit = { credentials: "include", headers: { "X-Secretary-CSRF": "1" } };

const CATS: { id: Category; label: MessageKey; icon: () => JSX.Element }[] = [
  { id: "looks", label: "editor.catLooks", icon: SparkleIcon },
  { id: "body", label: "editor.catBody", icon: BodyIcon },
  { id: "face", label: "editor.catFace", icon: FaceIcon },
  { id: "eyes", label: "editor.catEyes", icon: EyeIcon },
  { id: "skin", label: "editor.catSkin", icon: LipsIcon },
  { id: "hair", label: "editor.catHair", icon: HairIcon },
  { id: "accessories", label: "editor.catAccessories", icon: GemIcon },
  { id: "outfit", label: "editor.catOutfit", icon: HangerIcon },
];

const SLOT_LABELS: Record<AccessorySlot, MessageKey> = {
  head: "editor.slotHead",
  hair: "editor.slotHair",
  ears: "editor.slotEars",
  eyes: "editor.slotEyes",
  neck: "editor.slotNeck",
  back: "editor.slotBack",
  wristL: "editor.slotWristL",
  wristR: "editor.slotWristR",
};

const PART_LABELS: Record<PartSlot, MessageKey> = {
  top: "editor.slotTop",
  bottom: "editor.slotBottom",
  onepiece: "editor.slotOnepiece",
  legwear: "editor.slotLegwear",
  shoes: "editor.slotShoes",
  gloves: "editor.slotGloves",
};

const MAKEUP_LABELS: Record<MakeupLayer, MessageKey> = {
  lipstick: "editor.lipstick",
  eyeliner: "editor.eyeliner",
  eyeshadow: "editor.eyeshadow",
  blush: "editor.blush",
};

const RANDOM_CATS: Category[] = ["body", "face", "eyes", "skin", "hair", "accessories"];

function label(def: Pick<SliderDef, "label">): string {
  const l = locale();
  return def.label[l] ?? def.label.en;
}

function entryName(e: { id: string; name?: unknown }): string {
  const n = e.name;
  if (typeof n === "string") return n;
  const o = (n && typeof n === "object" ? n : {}) as Record<string, string>;
  return o[locale()] ?? o.en ?? Object.values(o)[0] ?? e.id;
}

/** The slider list of one category (body or face). */
function Sliders(props: { defs: readonly SliderDef[]; values: Record<string, number>; onInput: (id: string, v: number) => void; onCommit: () => void }) {
  return (
    <div class="ed-sliders">
      <For each={props.defs}>
        {(def) => {
          const value = () => props.values[def.id] ?? 50;
          return (
            <label class="ed-slider">
              <span class="ed-slider-name" onDblClick={() => (props.onInput(def.id, 50), props.onCommit())} title={t("editor.reset")}>
                {label(def)}
              </span>
              <input
                type="range"
                min="0"
                max="100"
                step="1"
                value={value()}
                onInput={(e) => props.onInput(def.id, +e.currentTarget.value)}
                onChange={() => props.onCommit()}
              />
              <output class="ed-slider-value" classList={{ "is-default": value() === 50 }}>
                {value()}
              </output>
            </label>
          );
        }}
      </For>
    </div>
  );
}

/** A tile of a part grid (thumbnail or glyph, name ribbon, frame when selected). */
function Tile(props: { name: string; thumbnail?: string | null; on: boolean; glyph: () => JSX.Element; onClick: () => void }) {
  const [broken, setBroken] = createSignal(false);
  return (
    <button type="button" class="wtile ed-tile" classList={{ "wtile--on": props.on }} aria-pressed={props.on ? "true" : "false"} onClick={() => props.onClick()}>
      <span class="wtile-art">
        <Show when={props.thumbnail && !broken()} fallback={<span class="wtile-glyph">{props.glyph()}</span>}>
          <img src={props.thumbnail!} alt="" loading="lazy" decoding="async" draggable={false} onError={() => setBroken(true)} />
        </Show>
      </span>
      <span class="wtile-name">{props.name}</span>
      <Show when={props.on}>
        <span class="wtile-check" aria-hidden="true">
          <CheckIcon />
        </span>
      </Show>
    </button>
  );
}

export default function LucinaEditor() {
  const [mod, setMod] = createSignal<StageModule | null>(null);
  const [mode, setMode] = createSignal<"loading" | "ready" | "fallback">("loading");
  const [look, setLook] = createSignal<Look | null>(null);
  const [baseline, setBaseline] = createSignal<string>("");
  const [cat, setCat] = createSignal<Category>("looks");
  const [slot, setSlot] = createSignal<AccessorySlot>("head");
  const [partSlot, setPartSlot] = createSignal<PartSlot>("top");
  const [history, setHistory] = createSignal({ undo: false, redo: false });
  const [editingId, setEditingId] = createSignal<string | null>(avatarSettings().current);
  const [naming, setNaming] = createSignal<{ mode: "new" | "rename"; id?: string; value: string } | null>(null);
  const [confirmClose, setConfirmClose] = createSignal(false);
  const [confirmDelete, setConfirmDelete] = createSignal<SavedLook | null>(null);
  const [note, setNote] = createSignal<string | null>(null);
  const [randomCats, setRandomCats] = createSignal<Category[]>(RANDOM_CATS);
  let canvas: HTMLCanvasElement | undefined;
  let stage: WardrobeStage | null = null;
  let editor: AvatarEditor | null = null;
  let disposed = false;
  let lookNow: Look | null = null;

  const catalog = () => ({ accessories: wardrobeAccessories(), parts: wardrobeParts() });
  const accessories = createMemo<AccessoryEntry[]>(() => mod()?.parseAccessories(wardrobeAccessories()) ?? []);
  const parts = createMemo<PartEntry[]>(() => mod()?.parseParts(wardrobeParts()) ?? []);
  const dirty = () => {
    const l = look();
    return !!l && JSON.stringify(l) !== baseline();
  };
  const editingLook = () => avatarSettings().looks.find((l) => l.id === editingId()) ?? null;

  function sync(): void {
    if (!editor) return;
    setLook(editor.look);
    setHistory({ undo: editor.canUndo, redo: editor.canRedo });
  }

  onMount(async () => {
    try {
      const m = await loadStage();
      if (disposed) return;
      setMod(m);
      lookNow = m.normalizeLook(currentLook() ?? {});
      setBaseline(JSON.stringify(lookNow));
      if (!canvas) throw new Error("canvas missing");
      const ctx = () => ({ assetBase: ASSET_BASE, fetchInit: AUTH, catalog: catalog() });
      const s = await m.createWardrobeStage(canvas, {
        modelUrl: MODEL_URL,
        manifestUrl: MANIFEST_URL,
        clipsUrl: CLIPS_URL,
        clipsMetaUrl: CLIPS_META_URL,
        fetchInit: AUTH,
        wardrobe: wardrobeOutfits(),
        outfit: currentOutfit()?.id ?? null,
        hairstyles: wardrobeHairstyles(),
        hairColors: wardrobeColors(),
        hair: currentHair(),
        hairColor: currentHairColor(),
        assetBase: ASSET_BASE,
        framing: "full",
        muted: true,
        muteStorageKey: null,
        changeEffect: false,
        interactive: true,
        environment: { quality: stageQuality(), theme: effectiveTheme() },
        prepareModel: m.editorPrepareModel(() => lookNow, ctx),
      });
      if (disposed) {
        s.dispose();
        return;
      }
      stage = s;
      editor = m.createAvatarEditor(s, { look: lookNow, ...ctx() });
      editor.onChange((l) => {
        lookNow = l;
        sync();
      });
      sync();
      editor.focus("looks");
      setMode("ready");
    } catch (e) {
      console.warn("[editor] stage not started:", e);
      if (!disposed) setMode("fallback");
    }
  });

  onCleanup(() => {
    disposed = true;
    editor?.dispose();
    stage?.dispose();
    editor = null;
    stage = null;
  });

  const update = (change: (l: Look) => void) => editor?.update(change);
  const commit = () => editor?.commit();

  function pick(c: Category) {
    setCat(c);
    if (c === "accessories") editor?.focus(c, slot());
    else editor?.focus(c);
  }

  function pickSlot(s: AccessorySlot) {
    setSlot(s);
    editor?.focus("accessories", s);
  }

  async function save(name: string, id?: string) {
    const l = editor?.look;
    if (!l) return;
    const saved = await saveLook(name, l as unknown as Record<string, unknown>, id);
    if (saved) {
      setEditingId(saved);
      setBaseline(JSON.stringify(l));
      setNote(t("editor.saved"));
    } else setNote(t("editor.saveFailed"));
  }

  function saveCurrent() {
    const editing = editingLook();
    if (editing) void save(editing.name, editing.id);
    else setNaming({ mode: "new", value: "" });
  }

  function loadLook(saved: SavedLook | null) {
    const m = mod();
    if (!m || !editor) return;
    editor.load(saved ? m.normalizeLook(saved.look) : m.defaultLook());
    setEditingId(saved?.id ?? null);
    setBaseline(JSON.stringify(editor.look));
  }

  function close() {
    if (dirty()) setConfirmClose(true);
    else closeEditor();
  }

  const swatches = () => mod()?.SWATCHES;

  return (
    <div class="editor" role="dialog" aria-modal="true" aria-label={t("editor.title")}>
      <header class="editor-top">
        <button type="button" class="icon-btn" onClick={close} aria-label={t("editor.close")} title={t("editor.close")}>
          <CloseIcon />
        </button>
        <div class="editor-title">
          <h2>{t("editor.title")}</h2>
          <span class="editor-sub">
            {editingLook()?.name ?? t("editor.lookDefault")}
            <Show when={dirty()}>
              <span class="editor-dirty"> · {t("editor.unsaved")}</span>
            </Show>
          </span>
        </div>
        <div class="editor-actions">
          <button type="button" class="icon-btn" disabled={!history().undo} onClick={() => (editor?.undo(), sync())} aria-label={t("editor.undo")} title={t("editor.undo")}>
            <UndoIcon />
          </button>
          <button type="button" class="icon-btn" disabled={!history().redo} onClick={() => (editor?.redo(), sync())} aria-label={t("editor.redo")} title={t("editor.redo")}>
            <RedoIcon />
          </button>
          <button
            type="button"
            class="icon-btn"
            disabled={mode() !== "ready"}
            onClick={() => (editor?.randomize({ categories: randomCats() }), sync())}
            aria-label={t("editor.random")}
            title={t("editor.random")}
          >
            <DiceIcon />
          </button>
          <button type="button" class="btn btn--primary editor-save" disabled={mode() !== "ready" || avatarSaving()} onClick={saveCurrent}>
            {t("editor.save")}
          </button>
        </div>
      </header>

      <div class="editor-body">
        <nav class="editor-rail" role="tablist" aria-label={t("editor.title")}>
          <For each={CATS}>
            {(c) => (
              <button
                type="button"
                role="tab"
                class="editor-cat"
                classList={{ "is-on": cat() === c.id }}
                aria-selected={cat() === c.id ? "true" : "false"}
                onClick={() => pick(c.id)}
              >
                <span class="editor-cat-icon">{c.icon()}</span>
                <span class="editor-cat-name">{t(c.label)}</span>
              </button>
            )}
          </For>
        </nav>

        <div class="editor-stage">
          <Show when={mode() !== "fallback"} fallback={<p class="editor-nostage">{t("editor.noStage")}</p>}>
            <canvas ref={canvas} class="stage-canvas" role="img" aria-label={t("shell.lucinaCanvas")} />
          </Show>
          <Show when={mode() === "loading"}>
            <div class="stage-loading" role="status">
              <span class="holo-loader" aria-hidden="true" />
              <span>{t("editor.loading")}</span>
            </div>
          </Show>
          <Show when={note()}>
            {(n) => (
              <span class="stage-pill stage-pill--note" role="status" onAnimationEnd={() => setNote(null)}>
                {n()}
              </span>
            )}
          </Show>
        </div>

        <section class="editor-panel" role="tabpanel">
          <Show when={look()} fallback={<p class="editor-hint">{t("editor.loading")}</p>}>
            {(l) => (
              <Switch>
                <Match when={cat() === "looks"}>
                  <div class="ed-head">
                    <h3>{t("editor.catLooks")}</h3>
                    <span class="ed-count">{t("editor.looksCount", { count: avatarSettings().looks.length })}</span>
                  </div>
                  <div class="ed-looks">
                    <button type="button" class="ed-look" classList={{ "is-on": editingId() === null }} onClick={() => loadLook(null)}>
                      <span class="ed-look-name">{t("editor.lookDefault")}</span>
                      <Show when={avatarSettings().current === null}>
                        <span class="ed-badge">{t("editor.wearing")}</span>
                      </Show>
                    </button>
                    <For each={avatarSettings().looks}>
                      {(saved) => (
                        <div class="ed-look" classList={{ "is-on": editingId() === saved.id }}>
                          <button type="button" class="ed-look-main" onClick={() => loadLook(saved)}>
                            <span class="ed-look-name">{saved.name}</span>
                            <Show when={avatarSettings().current === saved.id}>
                              <span class="ed-badge">{t("editor.wearing")}</span>
                            </Show>
                          </button>
                          <div class="ed-look-actions">
                            <Show when={avatarSettings().current !== saved.id}>
                              <button type="button" class="wchip" onClick={() => void wearLook(saved.id)}>
                                {t("editor.wear")}
                              </button>
                            </Show>
                            <button type="button" class="wchip" onClick={() => setNaming({ mode: "rename", id: saved.id, value: saved.name })}>
                              {t("editor.rename")}
                            </button>
                            <button type="button" class="icon-btn icon-btn--sm" aria-label={t("editor.delete")} title={t("editor.delete")} onClick={() => setConfirmDelete(saved)}>
                              <TrashIcon />
                            </button>
                          </div>
                        </div>
                      )}
                    </For>
                  </div>
                  <Show when={!avatarSettings().looks.length}>
                    <p class="editor-hint">{t("editor.noLooks")}</p>
                  </Show>
                  <div class="ed-row">
                    <button type="button" class="btn" disabled={avatarSettings().looks.length >= 20} onClick={() => setNaming({ mode: "new", value: "" })}>
                      {t("editor.saveAsNew")}
                    </button>
                    <button type="button" class="btn" onClick={() => (editor?.reset(), sync())}>
                      <RefreshIcon /> {t("editor.resetAll")}
                    </button>
                  </div>
                  <fieldset class="ed-random">
                    <legend>{t("editor.randomWhat")}</legend>
                    <div class="ed-chips">
                      <For each={RANDOM_CATS}>
                        {(c) => (
                          <button
                            type="button"
                            class="wchip"
                            classList={{ "wchip--on": randomCats().includes(c) }}
                            aria-pressed={randomCats().includes(c) ? "true" : "false"}
                            onClick={() => setRandomCats(randomCats().includes(c) ? randomCats().filter((x) => x !== c) : [...randomCats(), c])}
                          >
                            {t(CATS.find((x) => x.id === c)!.label)}
                          </button>
                        )}
                      </For>
                    </div>
                    <button type="button" class="btn btn--primary" disabled={!randomCats().length} onClick={() => (editor?.randomize({ categories: randomCats() }), sync())}>
                      <DiceIcon /> {t("editor.random")}
                    </button>
                  </fieldset>
                </Match>

                <Match when={cat() === "body"}>
                  <div class="ed-head">
                    <h3>{t("editor.catBody")}</h3>
                    <button type="button" class="wchip" onClick={() => (editor?.reset("body"), sync())}>
                      {t("editor.reset")}
                    </button>
                  </div>
                  <Sliders defs={mod()!.BODY_SLIDERS} values={l().body} onInput={(id, v) => update((x) => (x.body[id] = v))} onCommit={commit} />
                </Match>

                <Match when={cat() === "face"}>
                  <div class="ed-head">
                    <h3>{t("editor.catFace")}</h3>
                    <button type="button" class="wchip" onClick={() => (editor?.reset("face"), sync())}>
                      {t("editor.reset")}
                    </button>
                  </div>
                  <Sliders defs={mod()!.FACE_SLIDERS} values={l().face} onInput={(id, v) => update((x) => (x.face[id] = v))} onCommit={commit} />
                </Match>

                <Match when={cat() === "eyes"}>
                  <div class="ed-head">
                    <h3>{t("editor.catEyes")}</h3>
                    <button type="button" class="wchip" onClick={() => (editor?.reset("eyes"), sync())}>
                      {t("editor.reset")}
                    </button>
                  </div>
                  <ColorWheel
                    label={t("editor.iris")}
                    value={l().eyes.iris}
                    fallback="#3d6fd1"
                    natural
                    swatches={swatches()?.iris}
                    onInput={(hex) => update((x) => (x.eyes.iris = hex))}
                    onChange={commit}
                  />
                  <label class="ed-switch">
                    <input type="checkbox" checked={l().eyes.highlight} onChange={(e) => (update((x) => (x.eyes.highlight = e.currentTarget.checked)), commit())} />
                    <span>{t("editor.highlight")}</span>
                  </label>
                </Match>

                <Match when={cat() === "skin"}>
                  <div class="ed-head">
                    <h3>{t("editor.catSkin")}</h3>
                    <button type="button" class="wchip" onClick={() => (editor?.reset("skin"), sync())}>
                      {t("editor.reset")}
                    </button>
                  </div>
                  <h4 class="ed-sub">{t("editor.skinTone")}</h4>
                  <ColorWheel
                    label={t("editor.skinTone")}
                    value={l().skin.tone}
                    fallback="#f6dccb"
                    natural
                    swatches={swatches()?.skin}
                    onInput={(hex) => update((x) => (x.skin.tone = hex))}
                    onChange={commit}
                  />
                  <For each={mod()!.MAKEUP_LAYERS as readonly MakeupLayer[]}>
                    {(layer) => (
                      <details class="ed-makeup">
                        <summary>
                          <span class="ed-dot" style={{ background: l().makeup[layer].color, opacity: 0.25 + l().makeup[layer].amount / 133 }} />
                          {t(MAKEUP_LABELS[layer])}
                          <span class="ed-slider-value">{l().makeup[layer].amount}</span>
                        </summary>
                        <label class="ed-slider">
                          <span class="ed-slider-name">{t("editor.strength")}</span>
                          <input
                            type="range"
                            min="0"
                            max="100"
                            value={l().makeup[layer].amount}
                            onInput={(e) => update((x) => (x.makeup[layer].amount = +e.currentTarget.value))}
                            onChange={commit}
                          />
                          <output class="ed-slider-value">{l().makeup[layer].amount}</output>
                        </label>
                        <ColorWheel
                          label={t(MAKEUP_LABELS[layer])}
                          value={l().makeup[layer].color}
                          swatches={swatches()?.[layer]}
                          onInput={(hex) =>
                            update((x) => {
                              x.makeup[layer].color = hex ?? x.makeup[layer].color;
                              if (!x.makeup[layer].amount) x.makeup[layer].amount = 50;
                            })
                          }
                          onChange={commit}
                        />
                      </details>
                    )}
                  </For>
                </Match>

                <Match when={cat() === "hair"}>
                  <div class="ed-head">
                    <h3>{t("editor.hairstyle")}</h3>
                    <button type="button" class="wchip" onClick={() => (editor?.reset("hair"), sync())}>
                      {t("editor.reset")}
                    </button>
                  </div>
                  <div class="wardrobe-grid ed-grid">
                    <For each={hairChoices(wardrobeHairstyles())}>
                      {(h) => (
                        <Tile
                          name={outfitName(h, locale())}
                          thumbnail={thumbnailUrl(h)}
                          on={(l().hair.style ?? currentHair()) === h.id}
                          glyph={HairIcon}
                          onClick={() => (update((x) => (x.hair.style = h.id)), commit())}
                        />
                      )}
                    </For>
                  </div>
                  <h4 class="ed-sub">{t("editor.hairColor")}</h4>
                  <ColorWheel
                    label={t("editor.hairColor")}
                    value={l().hair.color}
                    fallback="#2b3a67"
                    natural
                    swatches={swatches()?.hair}
                    onInput={(hex) => update((x) => (x.hair.color = hex))}
                    onChange={commit}
                  />
                  <h4 class="ed-sub">{t("editor.hairHighlight")}</h4>
                  <ColorWheel
                    label={t("editor.hairHighlight")}
                    value={l().hair.highlight}
                    fallback="#9fb0e0"
                    natural
                    swatches={swatches()?.highlight}
                    onInput={(hex) => update((x) => (x.hair.highlight = hex))}
                    onChange={commit}
                  />
                </Match>

                <Match when={cat() === "accessories"}>
                  <div class="ed-head">
                    <h3>{t("editor.catAccessories")}</h3>
                    <button type="button" class="wchip" onClick={() => (editor?.reset("accessories"), sync())}>
                      {t("editor.reset")}
                    </button>
                  </div>
                  <div class="ed-chips" role="tablist">
                    <For each={mod()!.ACCESSORY_SLOTS as readonly AccessorySlot[]}>
                      {(s) => (
                        <button
                          type="button"
                          role="tab"
                          class="wchip"
                          classList={{ "wchip--on": slot() === s, "wchip--set": !!l().accessories[s] }}
                          aria-selected={slot() === s ? "true" : "false"}
                          onClick={() => pickSlot(s)}
                        >
                          {t(SLOT_LABELS[s])}
                        </button>
                      )}
                    </For>
                  </div>
                  <div class="wardrobe-grid ed-grid">
                    <Tile name={t("editor.none")} on={!l().accessories[slot()]} glyph={CloseIcon} onClick={() => (update((x) => delete x.accessories[slot()]), commit())} />
                    <For each={accessories().filter((a) => a.slot === slot())}>
                      {(a) => (
                        <Tile
                          name={entryName(a)}
                          thumbnail={a.thumbnail ? ASSET_BASE + a.thumbnail : null}
                          on={l().accessories[slot()] === a.id}
                          glyph={GemIcon}
                          onClick={() => (update((x) => (x.accessories[a.slot] = a.id)), commit())}
                        />
                      )}
                    </For>
                  </div>
                  <label class="ed-switch">
                    <input type="checkbox" checked={l().tiara} onChange={(e) => (update((x) => (x.tiara = e.currentTarget.checked)), commit())} />
                    <span>{t("editor.tiara")}</span>
                  </label>
                </Match>

                <Match when={cat() === "outfit"}>
                  <div class="ed-head">
                    <h3>{t("editor.outfitWhole")}</h3>
                    <button type="button" class="wchip" onClick={() => (editor?.reset("outfit"), sync())}>
                      {t("editor.reset")}
                    </button>
                  </div>
                  <div class="wardrobe-grid ed-grid">
                    <Tile
                      name={t("editor.outfitFromWardrobe")}
                      on={!l().outfit.id && !Object.keys(l().outfit.parts).length}
                      glyph={DropIcon}
                      onClick={() => (update((x) => (x.outfit = { id: null, parts: {} })), commit())}
                    />
                    <For each={wardrobeOutfits().filter((o) => o.id !== "base")}>
                      {(o) => (
                        <Tile
                          name={outfitName(o, locale())}
                          thumbnail={thumbnailUrl(o)}
                          on={l().outfit.id === o.id && !Object.keys(l().outfit.parts).length}
                          glyph={HangerIcon}
                          onClick={() => (update((x) => (x.outfit = { id: o.id, parts: {} })), commit())}
                        />
                      )}
                    </For>
                  </div>
                  <h4 class="ed-sub">{t("editor.outfitParts")}</h4>
                  <Show when={parts().length && wardrobeOutfits().some((o) => o.id === "base")} fallback={<p class="editor-hint">{t("editor.partsSoon")}</p>}>
                    <div class="ed-chips" role="tablist">
                      <For each={mod()!.PART_SLOTS as readonly PartSlot[]}>
                        {(s) => (
                          <button
                            type="button"
                            role="tab"
                            class="wchip"
                            classList={{ "wchip--on": partSlot() === s, "wchip--set": !!l().outfit.parts[s] }}
                            aria-selected={partSlot() === s ? "true" : "false"}
                            onClick={() => setPartSlot(s)}
                          >
                            {t(PART_LABELS[s])}
                          </button>
                        )}
                      </For>
                    </div>
                    <div class="wardrobe-grid ed-grid">
                      <Tile
                        name={t("editor.none")}
                        on={!l().outfit.parts[partSlot()]}
                        glyph={CloseIcon}
                        onClick={() => (update((x) => delete x.outfit.parts[partSlot()]), commit())}
                      />
                      <For each={parts().filter((p) => p.slot === partSlot())}>
                        {(p) => (
                          <Tile
                            name={entryName(p)}
                            thumbnail={p.thumbnail ? ASSET_BASE + p.thumbnail : null}
                            on={l().outfit.parts[partSlot()] === p.id}
                            glyph={HangerIcon}
                            onClick={() =>
                              (update((x) => {
                                const m = mod()!;
                                x.outfit.parts = m.resolveParts({ ...x.outfit.parts, [p.slot]: p.id }, parts(), p.slot);
                              }),
                              commit())
                            }
                          />
                        )}
                      </For>
                    </div>
                  </Show>
                </Match>
              </Switch>
            )}
          </Show>
        </section>
      </div>

      <Show when={naming()}>
        {(n) => (
          <div class="editor-modal" role="dialog" aria-modal="true" aria-label={t("editor.nameLabel")}>
            <form
              class="editor-modal-card"
              onSubmit={(e) => {
                e.preventDefault();
                const name = n().value.trim();
                if (!name) return;
                const cur = n();
                setNaming(null);
                if (cur.mode === "rename" && cur.id) {
                  const saved = avatarSettings().looks.find((l) => l.id === cur.id);
                  if (saved) void saveLook(name, saved.look, saved.id);
                } else void save(name);
              }}
            >
              <label>
                <span>{t("editor.nameLabel")}</span>
                <input
                  type="text"
                  maxLength={40}
                  autofocus
                  placeholder={t("editor.namePlaceholder")}
                  value={n().value}
                  onInput={(e) => setNaming({ ...n(), value: e.currentTarget.value })}
                />
              </label>
              <div class="ed-row">
                <button type="button" class="btn" onClick={() => setNaming(null)}>
                  {t("editor.keepEditing")}
                </button>
                <button type="submit" class="btn btn--primary" disabled={!n().value.trim()}>
                  {n().mode === "rename" ? t("editor.rename") : t("editor.save")}
                </button>
              </div>
            </form>
          </div>
        )}
      </Show>

      <Show when={confirmClose()}>
        <div class="editor-modal" role="alertdialog" aria-modal="true" aria-label={t("editor.discardTitle")}>
          <div class="editor-modal-card">
            <p>{t("editor.discardTitle")}</p>
            <div class="ed-row">
              <button type="button" class="btn" onClick={() => setConfirmClose(false)}>
                {t("editor.keepEditing")}
              </button>
              <button type="button" class="btn btn--danger" onClick={() => closeEditor()}>
                {t("editor.discard")}
              </button>
            </div>
          </div>
        </div>
      </Show>

      <Show when={confirmDelete()}>
        {(d) => (
          <div class="editor-modal" role="alertdialog" aria-modal="true" aria-label={t("editor.delete")}>
            <div class="editor-modal-card">
              <p>{t("editor.deleteConfirm", { name: d().name })}</p>
              <div class="ed-row">
                <button type="button" class="btn" onClick={() => setConfirmDelete(null)}>
                  {t("editor.keepEditing")}
                </button>
                <button
                  type="button"
                  class="btn btn--danger"
                  onClick={() => {
                    const id = d().id;
                    setConfirmDelete(null);
                    if (editingId() === id) setEditingId(null);
                    void deleteLook(id);
                  }}
                >
                  {t("editor.delete")}
                </button>
              </div>
            </div>
          </div>
        )}
      </Show>
    </div>
  );
}
