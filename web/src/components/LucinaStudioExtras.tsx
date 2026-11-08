import type { DecoratorLabels, StudioExtras, WardrobeStage } from "@secretary/lucina-stage";
import { createSignal, For, onCleanup, Show, type Accessor, type JSX } from "solid-js";
import { t } from "../i18n";
import { clearBackgroundImage, saveBackgroundImage, shrinkImage } from "../lib/studioImage";

// The studio extras panels (packages/lucina-stage/src/studio2): hands, pose editing, face
// sliders, props, camera, fun (flick ring, live stamps, dance moves) and the image background.

type Key = Parameters<typeof t>[0];

const HANDS: { id: string | null; label: Key }[] = [
  { id: null, label: "studio2.handClip" },
  { id: "relaxed", label: "studio2.handRelaxed" },
  { id: "fist", label: "studio2.handFist" },
  { id: "open", label: "studio2.handOpen" },
  { id: "peace", label: "studio2.handPeace" },
  { id: "point", label: "studio2.handPoint" },
  { id: "ok", label: "studio2.handOk" },
  { id: "heart", label: "studio2.handHeart" },
];
export const PROP_LABEL: Record<string, Key> = {
  chair: "studio2.propChair",
  table: "studio2.propTable",
  cup: "studio2.propCup",
  book: "studio2.propBook",
  plant: "studio2.propPlant",
  lamp: "studio2.propLamp",
  cushion: "studio2.propCushion",
  umbrella: "studio2.propUmbrella",
  guitar: "studio2.propGuitar",
  cat_plush: "studio2.propCatPlush",
};
const PROP_KINDS = Object.keys(PROP_LABEL);
const MAX_PROPS = 12;
const FILTERS: { id: string; label: Key }[] = [
  { id: "none", label: "studio2.filterNone" },
  { id: "sepia", label: "studio2.filterSepia" },
  { id: "mono", label: "studio2.filterMono" },
  { id: "vignette", label: "studio2.filterVignette" },
];
const GAZES: { id: "camera" | "ahead" | "custom"; label: Key }[] = [
  { id: "camera", label: "studio2.gazeCamera" },
  { id: "ahead", label: "studio2.gazeAhead" },
  { id: "custom", label: "studio2.gazeCustom" },
];
const FLICKS: { id: "off" | "hold" | "left"; label: Key }[] = [
  { id: "off", label: "studio2.flickOff" },
  { id: "hold", label: "studio2.flickHold" },
  { id: "left", label: "studio2.flickLeft" },
];
const DANCES: { id: string | null; label: Key }[] = [
  { id: null, label: "studio2.danceClip" },
  { id: "bounce", label: "studio2.danceBounce" },
  { id: "wave", label: "studio2.danceWave" },
  { id: "step", label: "studio2.danceStep" },
  { id: "twist", label: "studio2.danceTwist" },
  { id: "heart", label: "studio2.danceHeart" },
  { id: "mix", label: "studio2.danceMix" },
];

/** Labels for the photo decorator, in the app's language. */
export function decoratorLabels(): Partial<DecoratorLabels> {
  return {
    title: t("studio2.decoTitle"),
    frame: t("studio2.decoFrame"),
    stamp: t("studio2.decoStamp"),
    text: t("studio2.decoText"),
    paint: t("studio2.decoPaint"),
    undo: t("studio2.decoUndo"),
    redo: t("studio2.decoRedo"),
    save: t("studio2.decoSave"),
    cancel: t("studio2.decoCancel"),
    remove: t("studio2.decoRemove"),
    mirror: t("studio2.decoMirror"),
    size: t("studio2.decoSize"),
    turn: t("studio2.decoTurn"),
    opacity: t("studio2.decoOpacity"),
    outline: t("studio2.decoOutline"),
    font: t("studio2.decoFont"),
    add: t("studio2.decoAdd"),
    placeholder: t("studio2.decoPlaceholder"),
    brush: t("studio2.decoBrush"),
    eraser: t("studio2.decoEraser"),
    clear: t("studio2.decoClear"),
    frames: {
      none: t("studio2.frameNone"),
      polaroid: t("studio2.framePolaroid"),
      film: t("studio2.frameFilm"),
      hearts: t("studio2.frameHearts"),
      stars: t("studio2.frameStars"),
      neon: t("studio2.frameNeon"),
      lace: t("studio2.frameLace"),
      ribbon: t("studio2.frameRibbon"),
    },
    fonts: {
      sans: t("studio2.fontSans"),
      round: t("studio2.fontRound"),
      serif: t("studio2.fontSerif"),
      bold: t("studio2.fontBold"),
      hand: t("studio2.fontHand"),
      mono: t("studio2.fontMono"),
    },
  };
}

/** Labels for the flick ring's faces and motions. */
export function flickLabels(): Record<string, string> {
  return { smile: t("studio2.flickSmile"), greet: t("studio2.flickWave"), surprised: t("studio2.flickWow"), happy: t("studio2.flickHappy") };
}

/** Re-reads the extras whenever they change (their settings are plain objects). */
function useExtras(extras: Accessor<StudioExtras | null>): Accessor<number> {
  const [v, setV] = createSignal(0);
  const ex = extras();
  if (ex) onCleanup(ex.onChange(() => setV((n) => n + 1)));
  return v;
}

/** A slider that locks the camera while it is dragged (Custom Cast's camera lock). */
function Range(props: { label: string; min: number; max: number; step: number; value: number; stage: Accessor<WardrobeStage | null>; onInput: (v: number) => void }): JSX.Element {
  return (
    <label class="studio-range">
      <span>{props.label}</span>
      <input
        type="range"
        min={props.min}
        max={props.max}
        step={props.step}
        value={props.value}
        aria-label={props.label}
        onPointerDown={() => props.stage()?.setInteractive(false)}
        onPointerUp={() => props.stage()?.setInteractive(true)}
        onPointerCancel={() => props.stage()?.setInteractive(true)}
        onInput={(e) => props.onInput(Number(e.currentTarget.value))}
      />
    </label>
  );
}

function Chip(props: { pressed?: boolean; onClick: () => void; children: JSX.Element; label?: string }): JSX.Element {
  return (
    <button type="button" class="studio-chip" aria-pressed={props.pressed === undefined ? undefined : props.pressed ? "true" : "false"} aria-label={props.label} onClick={() => props.onClick()}>
      {props.children}
    </button>
  );
}

export function HandsPanel(props: { extras: Accessor<StudioExtras | null> }): JSX.Element {
  const v = useExtras(props.extras);
  const [side, setSide] = createSignal<"l" | "r" | "both">("both");
  const current = () => {
    v();
    const s = props.extras()?.settings.hands;
    if (!s) return null;
    return side() === "r" ? s.r : side() === "l" ? s.l : s.l === s.r ? s.l : undefined;
  };
  return (
    <>
      <div class="studio-row">
        <Chip pressed={side() === "l"} onClick={() => setSide("l")}>{t("studio2.left")}</Chip>
        <Chip pressed={side() === "both"} onClick={() => setSide("both")}>{t("studio2.both")}</Chip>
        <Chip pressed={side() === "r"} onClick={() => setSide("r")}>{t("studio2.right")}</Chip>
      </div>
      <div class="studio-row">
        <For each={HANDS}>
          {(h) => (
            <Chip pressed={current() === h.id} onClick={() => props.extras()?.setHand(side(), h.id as never)}>
              {t(h.label)}
            </Chip>
          )}
        </For>
      </div>
    </>
  );
}

export function PosePanel(props: { extras: Accessor<StudioExtras | null> }): JSX.Element {
  const v = useExtras(props.extras);
  const [name, setName] = createSignal("");
  const ex = () => (v(), props.extras());
  onCleanup(() => props.extras()?.editPose(false));
  return (
    <>
      <div class="studio-row">
        <Chip pressed={!!ex()?.editing} onClick={() => ex()?.editPose(!ex()?.editing)}>
          {ex()?.editing ? t("studio2.poseEditing") : t("studio2.poseEdit")}
        </Chip>
        <Chip onClick={() => ex()?.applyPose(null)}>{t("studio2.poseReset")}</Chip>
        <Show when={ex()?.editing}>
          <span class="studio-empty">{t("studio2.poseHint")}</span>
        </Show>
      </div>
      <form
        class="studio-row"
        onSubmit={(e) => {
          e.preventDefault();
          if (ex()?.savePose(name())) setName("");
        }}
      >
        <input class="studio-input" type="text" maxLength={40} placeholder={t("studio2.poseName")} aria-label={t("studio2.poseName")} value={name()} onInput={(e) => setName(e.currentTarget.value)} />
        <button type="submit" class="studio-chip" disabled={!name().trim()}>
          {t("studio2.poseSave")}
        </button>
      </form>
      <div class="studio-row">
        <For each={ex()?.settings.poses ?? []}>
          {(p) => (
            <Chip pressed={ex()?.activePose === p.name} onClick={() => ex()?.applyPose(p.name)}>
              {p.name}
              <Show when={ex()?.settings.idlePose === p.name}>
                <span class="studio-tag">{t("studio2.poseIdleOn")}</span>
              </Show>
            </Chip>
          )}
        </For>
        <Show when={ex()?.activePose}>
          {(n) => (
            <>
              <Chip pressed={ex()?.settings.idlePose === n()} onClick={() => ex()?.setIdlePose(ex()?.settings.idlePose === n() ? null : n())}>
                {t("studio2.poseIdle")}
              </Chip>
              <Chip onClick={() => ex()?.deletePose(n())}>{t("studio2.poseDelete")}</Chip>
            </>
          )}
        </Show>
      </div>
    </>
  );
}

export function FaceSliders(props: { extras: Accessor<StudioExtras | null>; stage: Accessor<WardrobeStage | null> }): JSX.Element {
  const v = useExtras(props.extras);
  const [name, setName] = createSignal("");
  const ex = () => (v(), props.extras());
  const morphs = () => props.extras()?.morphs ?? [];
  return (
    <>
      <div class="studio-row studio-sliders">
        <For each={morphs()}>
          {(m) => <Range label={m.replace(/_/g, " ")} min={0} max={1} step={0.05} value={ex()?.settings.face[m] ?? 0} stage={props.stage} onInput={(w) => ex()?.setMorph(m, w)} />}
        </For>
      </div>
      <form
        class="studio-row"
        onSubmit={(e) => {
          e.preventDefault();
          if (ex()?.saveFace(name())) setName("");
        }}
      >
        <input class="studio-input" type="text" maxLength={40} placeholder={t("studio2.faceName")} aria-label={t("studio2.faceName")} value={name()} onInput={(e) => setName(e.currentTarget.value)} />
        <button type="submit" class="studio-chip" disabled={!name().trim()}>
          {t("studio2.faceSave")}
        </button>
        <Chip onClick={() => ex()?.resetFace()}>{t("studio2.faceReset")}</Chip>
      </form>
      <div class="studio-row">
        <For each={ex()?.settings.faces ?? []}>{(f) => <Chip onClick={() => ex()?.applyFace(f.name)}>{f.name}</Chip>}</For>
      </div>
    </>
  );
}

export function PropsPanel(props: { extras: Accessor<StudioExtras | null>; stage: Accessor<WardrobeStage | null> }): JSX.Element {
  const v = useExtras(props.extras);
  const ex = () => (v(), props.extras());
  const [busy, setBusy] = createSignal(false);
  const full = () => (ex()?.settings.props.length ?? 0) >= MAX_PROPS;
  async function add(kind: string) {
    const e = props.extras();
    if (!e || full()) return;
    setBusy(true);
    try {
      await e.addProp(kind);
    } catch {
      // The props file could not be loaded; nothing was added.
    } finally {
      setBusy(false);
    }
  }
  const sel = () => ex()?.selectedProp ?? null;
  return (
    <>
      <div class="studio-row">
        <For each={PROP_KINDS}>
          {(k) => (
            <Chip onClick={() => void add(k)} label={`${t("studio2.propAdd")}: ${t(PROP_LABEL[k]!)}`}>
              + {t(PROP_LABEL[k]!)}
            </Chip>
          )}
        </For>
      </div>
      <Show when={busy()}>
        <span class="studio-empty">{t("studio2.propsLoading")}</span>
      </Show>
      <Show when={full()}>
        <span class="studio-empty">{t("studio2.propLimit", { count: MAX_PROPS })}</span>
      </Show>
      <Show when={sel()} fallback={<span class="studio-empty">{t("studio2.propNone")}</span>}>
        {(p) => (
          <div class="studio-row studio-sliders">
            <span class="studio-tag">{t(PROP_LABEL[p().kind] ?? "studio2.propSelected")}</span>
            <Range label={t("studio2.propTurn")} min={0} max={359} step={1} value={p().rot} stage={props.stage} onInput={(r) => ex()?.updateProp(p().id, { rot: r })} />
            <Range label={t("studio2.propSize")} min={0.25} max={3} step={0.05} value={p().scale} stage={props.stage} onInput={(s) => ex()?.updateProp(p().id, { scale: s })} />
            <Range label={t("studio2.propLift")} min={0} max={1.5} step={0.01} value={p().y} stage={props.stage} onInput={(y) => ex()?.updateProp(p().id, { y })} />
            <Chip onClick={() => ex()?.removeProp(p().id)}>{t("studio2.propRemove")}</Chip>
          </div>
        )}
      </Show>
      <Show when={(ex()?.settings.props.length ?? 0) > 0}>
        <div class="studio-row">
          <Chip onClick={() => ex()?.clearProps()}>{t("studio2.propClear")}</Chip>
        </div>
      </Show>
    </>
  );
}

export function CameraPanel(props: { extras: Accessor<StudioExtras | null>; stage: Accessor<WardrobeStage | null> }): JSX.Element {
  const v = useExtras(props.extras);
  const ex = () => (v(), props.extras());
  return (
    <>
      <div class="studio-row studio-sliders">
        <Range label={t("studio2.roll")} min={-45} max={45} step={1} value={ex()?.settings.roll ?? 0} stage={props.stage} onInput={(r) => ex()?.setRoll(r)} />
      </div>
      <div class="studio-row">
        <span class="studio-tag">{t("studio2.filter")}</span>
        <For each={FILTERS}>
          {(f) => (
            <Chip pressed={ex()?.settings.filter.id === f.id} onClick={() => ex()?.setFilter({ id: f.id as never })}>
              {t(f.label)}
            </Chip>
          )}
        </For>
        <Show when={ex()?.settings.filter.id !== "none"}>
          <Range label={t("studio2.strength")} min={0} max={1} step={0.05} value={ex()?.settings.filter.strength ?? 0.8} stage={props.stage} onInput={(s) => ex()?.setFilter({ strength: s })} />
        </Show>
      </div>
      <div class="studio-row">
        <span class="studio-tag">{t("studio2.gaze")}</span>
        <For each={GAZES}>
          {(g) => (
            <Chip pressed={ex()?.settings.gaze.mode === g.id} onClick={() => ex()?.setGaze({ mode: g.id })}>
              {t(g.label)}
            </Chip>
          )}
        </For>
      </div>
      <Show when={ex()?.settings.gaze.mode === "custom"}>
        <div class="studio-row studio-sliders">
          <Range label={t("studio2.gazeX")} min={-1} max={1} step={0.05} value={ex()?.settings.gaze.x ?? 0} stage={props.stage} onInput={(x) => ex()?.setGaze({ x })} />
          <Range label={t("studio2.gazeY")} min={-1} max={1} step={0.05} value={ex()?.settings.gaze.y ?? 0} stage={props.stage} onInput={(y) => ex()?.setGaze({ y })} />
        </div>
      </Show>
    </>
  );
}

export function FunPanel(props: { extras: Accessor<StudioExtras | null> }): JSX.Element {
  const v = useExtras(props.extras);
  const ex = () => (v(), props.extras());
  return (
    <>
      <div class="studio-row">
        <span class="studio-tag">{t("studio2.flick")}</span>
        <For each={FLICKS}>
          {(f) => (
            <Chip pressed={ex()?.settings.flick === f.id} onClick={() => ex()?.setFlick(f.id)}>
              {t(f.label)}
            </Chip>
          )}
        </For>
      </div>
      <Show when={ex()?.settings.flick !== "off"}>
        <span class="studio-empty">{t("studio2.flickHint")}</span>
      </Show>
      <div class="studio-row">
        <Chip pressed={!!ex()?.settings.liveStamps} onClick={() => ex()?.setLiveStamps(!ex()?.settings.liveStamps)}>
          {t("studio2.liveStamps")}
        </Chip>
        <Chip onClick={() => ex()?.popStamp(["heart", "star", "sparkle", "note", "flower", "crown"][Math.floor(Math.random() * 6)]!)}>{t("studio2.stampNow")}</Chip>
      </div>
      <div class="studio-row">
        <span class="studio-tag">{t("studio2.danceMove")}</span>
        <For each={DANCES}>
          {(d) => (
            <Chip pressed={(ex()?.settings.danceMove ?? null) === d.id} onClick={() => ex()?.setDanceMove(d.id as never)}>
              {t(d.label)}
            </Chip>
          )}
        </For>
      </div>
    </>
  );
}

/** "Your image" and "Remove image" for the Background tab. The picture never leaves the device. */
export function BackgroundImageChips(props: { extras: Accessor<StudioExtras | null>; selected: boolean; onChosen: () => void; onRemoved: () => void }): JSX.Element {
  let input: HTMLInputElement | undefined;
  async function pick(file: File | undefined) {
    const e = props.extras();
    if (!file || !e) return;
    const blob = await shrinkImage(file);
    try {
      await e.setBackgroundImage(blob);
      await saveBackgroundImage(blob);
      props.onChosen();
    } catch {
      // Not an image the browser can read.
    }
  }
  return (
    <>
      <button type="button" class="studio-chip" aria-pressed={props.selected ? "true" : "false"} onClick={() => input?.click()} title={t("studio2.bgImagePrivate")}>
        {t("studio2.bgImage")}
      </button>
      <input ref={input} type="file" accept="image/*" hidden onChange={(e) => void pick(e.currentTarget.files?.[0])} />
      <Show when={props.selected}>
        <button
          type="button"
          class="studio-chip"
          onClick={() => {
            void props.extras()?.setBackgroundImage(null);
            void clearBackgroundImage();
            props.onRemoved();
          }}
        >
          {t("studio2.bgImageRemove")}
        </button>
      </Show>
    </>
  );
}
