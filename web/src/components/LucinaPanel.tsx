import type { Framing, StudioExtras, Voice, WardrobeStage } from "@secretary/lucina-stage";
import { createEffect, createSignal, Match, on, onCleanup, onMount, Show, Switch } from "solid-js";
import { locale, speechLang, t } from "../i18n";
import { chatSessionId } from "../lib/ids";
import { PcmPlayer } from "../lib/pcmPlayer";
import { useVoiceSocket } from "../lib/useVoiceSocket";
import { runCommand, type CommandFailure, type CommandRun } from "../lib/voiceCommand";
import "../styles/voicein.css";
import { readString, writeString } from "../lib/storage";
import { loadStage, type StageModule } from "../lib/stageLoader";
import { isFaceTrackingSupported, startFaceTracking, type FaceTracker, type FaceTrackError } from "../lib/faceTrack";
import { effectiveTheme, stageQuality } from "../state/appearance";
import { onLucinaReply } from "../state/chat";
import { lucinaActivity, setLucinaActivity, type LucinaActivity } from "../state/stage";
import { ASSET_BASE } from "../lib/wardrobe";
import {
  avatarReady,
  currentHair,
  currentLook,
  currentHairColor,
  currentOutfit,
  openEditor,
  openWardrobe,
  outfitChanging,
  setOutfitChanging,
  wardrobeAccessories,
  wardrobeColors,
  wardrobeHairstyles,
  wardrobeOutfits,
  wardrobeParts,
} from "../state/wardrobe";
import { CloseIcon, CollapseIcon, EditIcon, HangerIcon, MicIcon, SparkleIcon, VolumeIcon, VolumeOffIcon } from "./Icons";
import { LucinaStudio, restoreStudio, savePhoto } from "./LucinaStudio";
import { decoratorLabels, flickLabels } from "./LucinaStudioExtras";

/** Same key the stage persists its mute preference under (MUTE_STORAGE_KEY). */
export const MUTE_KEY = "lucina-stage:muted";
const HINT_KEY = "secretary.stage.hintSeen";
const HINT_MS = 8_000;
/** Served behind the session (never public): the model may only be used privately. */
export const MODEL_URL = "/v1/assets/lucina.glb";
export const MANIFEST_URL = "/v1/assets/manifest.json";
/** Body animation clips (same skeleton as lucina.glb). Missing file = procedural idle. */
export const CLIPS_URL = "/v1/assets/lucina-anims.glb";
export const CLIPS_META_URL = "/v1/assets/clips.json";
export const TTS_URL = "/v1/tts";
/** The studio's starter props (original art, public): web/public/studio/. */
export const PROPS_URL = "/studio/lucina-props.glb";
const AUTH: RequestInit = { credentials: "include", headers: { "X-Secretary-CSRF": "1" } };

/** Where the editor's props and parts load from, and the catalogue it may use. */
function editorContext() {
  return { assetBase: ASSET_BASE, fetchInit: AUTH, catalog: { accessories: wardrobeAccessories(), parts: wardrobeParts() } };
}

/** The saved look she wears now, normalized by the stage (the model as authored when none). */
function wornLook(mod: StageModule) {
  return mod.normalizeLook?.(currentLook() ?? {}) ?? null;
}

/** Muted unless the owner has unmuted before: sound must never come as a surprise. */
export function storedMuted(): boolean {
  return readString("local", MUTE_KEY) !== "0";
}

type Mode = "loading" | "ready" | "fallback";

const WAVE_BARS = 14;
/** Voice-line keys of touch reactions (packages/lucina-stage/src/interact/reactions.ts). */
const TOUCH_LINES: Record<string, Parameters<typeof t>[0]> = {
  head: "shell.touchHead",
  face: "shell.touchFace",
  chest: "shell.touchChest",
  hips: "shell.touchHips",
  skirt: "shell.touchSkirt",
  arm: "shell.touchArm",
  hand: "shell.touchHand",
  leg: "shell.touchLeg",
  pat: "shell.touchPat",
  pat_more: "shell.touchPatMore",
  pat_love: "shell.touchPatLove",
  enough: "shell.touchEnough",
};
const STATE_LABELS: Record<LucinaActivity, Parameters<typeof t>[0]> = {
  idle: "shell.stateIdle",
  listening: "shell.stateListening",
  thinking: "shell.stateThinking",
  speaking: "shell.stateSpeaking",
};

/**
 * Lucina on a three.js stage. She thinks while a message is in flight, speaks
 * replies aloud when unmuted, and listens when tapped. The shell keeps one
 * instance mounted for the whole session; unmounting disposes the renderer
 * (frees the GPU).
 */
export function LucinaPanel(props: {
  /** "full" in the desktop side column, "bust" in the compact mobile views. */
  framing: Framing;
  /** A message is waiting for Lucina's reply. */
  thinking: boolean;
  /** Send a voice transcript as a chat message. */
  onTranscript: (text: string) => void;
  /** Voice input is not available: move focus to the text composer. */
  onTypeInstead: () => void;
  /** Hide the stage and free the GPU. */
  onCollapse: () => void;
  /** Mobile float: shrink back to the avatar button (the stage stays loaded). */
  onMinimize?: () => void;
}) {
  const [mode, setMode] = createSignal<Mode>("loading");
  const [muted, setMuted] = createSignal(storedMuted());
  const [listening, setListening] = createSignal(false);
  const [note, setNote] = createSignal<string | null>(null);
  const [hint, setHint] = createSignal(false);
  const [studioOpen, setStudioOpen] = createSignal(false);
  const [photo, setPhoto] = createSignal(false);
  const [stageRef, setStageRef] = createSignal<WardrobeStage | null>(null);
  /** Face tracking (F1): off, asking for the camera / loading the model, or mirroring. */
  const [face, setFace] = createSignal<"off" | "starting" | "on">("off");
  let tracker: FaceTracker | null = null;
  const [extrasRef, setExtrasRef] = createSignal<StudioExtras | null>(null);
  let canvas: HTMLCanvasElement | undefined;
  let mod: StageModule | null = null;
  let stage: WardrobeStage | null = null;
  let voice: Voice | null = null;
  let listenCtrl: AbortController | null = null;
  let ttsCtrl: AbortController | null = null;
  let hintTimer: ReturnType<typeof setTimeout> | undefined;
  let disposed = false;
  // Streaming voice (lib/voiceCommand.ts): the mic goes up the voice socket as it is captured, her
  // reply text and voice come back as they are written. Older servers: the Web Speech path below.
  const { socket } = useVoiceSocket();
  let command: CommandRun | null = null;
  let liveTimer: ReturnType<typeof setTimeout> | undefined;
  const [working, setWorking] = createSignal(false);
  const [liveText, setLiveText] = createSignal("");
  const player = new PcmPlayer({
    onPlaying: (playing) => {
      const s = stage;
      if (!s) return;
      if (playing) s.setState("speaking");
      else if (s.state === "speaking") s.setState(props.thinking || working() ? "thinking" : "idle");
    },
  });

  function showHintOnce() {
    if (readString("local", HINT_KEY)) return;
    writeString("local", HINT_KEY, "1");
    setHint(true);
    hintTimer = setTimeout(() => setHint(false), HINT_MS);
  }

  onMount(async () => {
    try {
      // Start in the right outfit when the wardrobe answers quickly (or is cached).
      const [m] = await Promise.all([loadStage(), avatarReady(1500)]);
      mod = m;
      if (disposed) return;
      voice = mod.createVoice({ ttsUrl: TTS_URL, fetchInit: AUTH, lang: speechLang() });
      if (!canvas) throw new Error("canvas missing");
      const s = await mod.createWardrobeStage(canvas, {
        modelUrl: MODEL_URL,
        wardrobe: wardrobeOutfits(),
        outfit: currentOutfit()?.id ?? null,
        hairstyles: wardrobeHairstyles(),
        hairColors: wardrobeColors(),
        hair: currentHair(),
        hairColor: currentHairColor(),
        assetBase: ASSET_BASE,
        manifestUrl: MANIFEST_URL,
        clipsUrl: CLIPS_URL,
        clipsMetaUrl: CLIPS_META_URL,
        fetchInit: AUTH,
        framing: props.framing,
        muteStorageKey: MUTE_KEY,
        muted: true,
        // Sky, holo floor, particles, rim light, bloom on "high" (packages/lucina-stage/src/scene).
        environment: { quality: stageQuality(), theme: effectiveTheme() },
        // Drag turns her, pinch/wheel zooms, double-tap resets, taps on her body react.
        interactive: true,
        // Her saved look (body, face, colours, accessories) on every model the wardrobe loads.
        prepareModel: mod.editorPrepareModel?.(() => wornLook(mod!), editorContext),
      });
      if (disposed) {
        s.dispose();
        return;
      }
      stage = s;
      setStageRef(s);
      // Keep Lucina's original procedural stage and model; the studio extras (hands, poses, props,
      // camera roll and filters, flick ring: packages/lucina-stage/src/studio2) sit on top of it.
      s.setBackground("default");
      const ex = mod.attachStudioExtras?.(s, { propsUrl: PROPS_URL, flickLabels: flickLabels() }) ?? null;
      setExtrasRef(ex);
      restoreStudio(s, ex);
      s.setMuted(muted());
      s.onStateChange((next) => setLucinaActivity(next));
      s.onChanging(setOutfitChanging);
      s.onTouch((e) => {
        setHint(false);
        void sayLine(e.line);
      });
      if (props.thinking) s.setState("thinking");
      if (import.meta.env.DEV) (window as unknown as { __lucina?: unknown }).__lucina = { stage: s, mod };
      // The wardrobe may have answered while the stage was loading.
      dress(s);
      setMode("ready");
    } catch {
      // No WebGL (or the stage failed to start): show her portrait instead.
      if (disposed) return;
      setMode("fallback");
    }
    showHintOnce();
  });

  onCleanup(() => {
    disposed = true;
    stopFace();
    clearTimeout(hintTimer);
    clearTimeout(liveTimer);
    command?.cancel();
    command = null;
    player.dispose();
    listenCtrl?.abort();
    ttsCtrl?.abort();
    extrasRef()?.dispose();
    setExtrasRef(null);
    stage?.dispose();
    stage = null;
    setStageRef(null);
    setLucinaActivity("idle");
    setOutfitChanging(false);
  });

  /**
   * Her look follows the wardrobe (section rules, pins, Lucina's own changes). A failed load
   * keeps what she has on; the next change tries again.
   */
  function dress(s: WardrobeStage): void {
    s.setWardrobe(wardrobeOutfits());
    s.setHairstyles(wardrobeHairstyles(), wardrobeColors());
    const warn = (what: string) => (e: unknown) => console.warn(`[wardrobe] ${what} not loaded:`, e);
    const outfit = currentOutfit()?.id;
    if (outfit && s.outfit !== outfit) s.setOutfit(outfit).catch(warn("outfit"));
    if (s.hair !== currentHair()) s.setHair(currentHair()).catch(warn("hairstyle"));
    if (s.hairColor !== currentHairColor()) s.setHairColor(currentHairColor()).catch(warn("hair colour"));
  }

  createEffect(
    on(
      () => [currentOutfit()?.id, currentHair(), currentHairColor(), wardrobeOutfits(), wardrobeHairstyles(), wardrobeColors()] as const,
      () => stage && dress(stage),
    ),
  );

  // A different saved look (picked in the editor, or by Lucina): sliders, colours and props change
  // in place; the outfit, hairstyle and hair colour it sets go through `dress` above.
  createEffect(
    on(
      () => [currentLook(), wardrobeAccessories(), wardrobeParts()] as const,
      () => {
        const s = stage;
        const look = mod ? wornLook(mod) : null;
        if (!s || !mod?.applyLook || !look) return;
        void mod.applyLook(s.three.model, look, editorContext()).then(() => s.refit?.());
      },
      { defer: true },
    ),
  );

  createEffect(on(effectiveTheme, (theme) => stage?.setTheme(theme), { defer: true }));
  createEffect(on(stageQuality, (q) => stage?.setQuality(q), { defer: true }));

  createEffect(
    on(
      () => props.thinking,
      (thinking) => {
        if (!stage || listening()) return;
        if (thinking) stage.setState("thinking");
        else if (stage.state === "thinking") stage.setState("idle");
      },
      { defer: true },
    ),
  );

  createEffect(
    on(
      () => props.framing,
      (framing) => stage?.setFraming(framing),
      { defer: true },
    ),
  );

  /** Speaks a reply, only when unmuted (muted: no TTS request, no audio at all). */
  async function say(text: string) {
    const s = stage;
    if (!s || !voice || muted()) return;
    ttsCtrl?.abort();
    const ctrl = new AbortController();
    ttsCtrl = ctrl;
    try {
      const audio = await voice.speak(text, { lang: speechLang(), signal: ctrl.signal });
      if (stage !== s || muted() || ctrl.signal.aborted) return;
      await s.speak(audio);
    } catch {
      // A failed TTS request leaves the reply as text only.
    } finally {
      if (ttsCtrl === ctrl) ttsCtrl = null;
    }
  }

  onMount(() => onCleanup(onLucinaReply((text) => void say(text))));

  /** Her short line for a touch: only when unmuted, never over a reply, cached after the first TTS. */
  const lineAudio = new Map<string, Blob>();
  async function sayLine(key: string) {
    const s = stage;
    const text = TOUCH_LINES[key] ? t(TOUCH_LINES[key]) : null;
    if (!s || !voice || !text || muted() || s.speaking || ttsCtrl || listening()) return;
    try {
      let audio = lineAudio.get(text);
      if (!audio) {
        audio = await voice.speak(text, { lang: speechLang() });
        lineAudio.set(text, audio);
      }
      if (stage !== s || muted() || s.speaking) return;
      await s.speak(audio);
    } catch {
      // No voice for this one: the reaction is enough.
    }
  }

  function toggleMute() {
    const next = !muted();
    setMuted(next);
    writeString("local", MUTE_KEY, next ? "1" : "0");
    stage?.setMuted(next);
    if (next) {
      ttsCtrl?.abort();
      stage?.stopSpeaking();
      player.stop();
    } else {
      // Inside the click: lets iOS start audio output later.
      void stage?.unlockAudio().catch(() => {});
    }
  }

  /** The frame as a PNG, with the camera filter when the extras are on. */
  async function shoot(): Promise<Blob | null> {
    const s = stage;
    if (!s) return null;
    const ex = extrasRef();
    return ex ? ex.snapshot() : s.snapshot();
  }

  async function takePhoto() {
    try {
      const shot = await shoot();
      if (!shot) return;
      const result = await savePhoto(shot);
      if (result !== "cancelled") setNote(t("studio.photoSaved"));
    } catch {
      // The frame could not be read back: nothing to save.
    }
  }

  const FACE_NOTES: Record<FaceTrackError, Parameters<typeof t>[0]> = {
    unsupported: "studio.faceUnsupported",
    denied: "studio.faceDenied",
    "no-camera": "studio.faceNoCamera",
    failed: "studio.faceFailed",
  };

  /**
   * Face tracking: only from a tap (the browser asks for the camera then). The frames stay in this
   * tab (MediaPipe runs here); it stops on Stop, when the page is hidden, or when she is closed.
   */
  async function toggleFace() {
    if (face() !== "off") {
      stopFace();
      return;
    }
    const s = stage;
    if (!s || !mod) return;
    setFace("starting");
    setNote(null);
    try {
      const started = await startFaceTracking({
        solver: new mod.FaceSolver(),
        onDrive: (d) => stage?.setFaceDrive(d),
        onStop: (reason) => {
          tracker = null;
          setFace("off");
          if (reason !== "ended" && reason !== "hidden") setNote(t(FACE_NOTES[reason]));
        },
      });
      if (disposed || stage !== s || face() !== "starting") {
        started.stop();
        return;
      }
      tracker = started;
      setFace("on");
      s.cameraPreset("face");
    } catch (e) {
      if (face() === "starting") setFace("off");
      setNote(t(FACE_NOTES[(typeof e === "string" && e in FACE_NOTES ? e : "failed") as FaceTrackError]));
    }
  }

  function stopFace() {
    if (face() === "off" && !tracker) return;
    tracker?.stop();
    tracker = null;
    setFace("off");
    stage?.setFaceDrive(null);
  }

  /** Photo, then frames, stamps, text and paint (studio P4), then save or share. */
  async function decoratePhoto() {
    const ex = extrasRef();
    if (!ex) return;
    try {
      const shot = await shoot();
      if (!shot) return;
      const done = await ex.decorate(shot, { labels: decoratorLabels() });
      if (!done) return;
      const result = await savePhoto(done);
      if (result !== "cancelled") setNote(t("studio.photoSaved"));
    } catch {
      // Nothing to save.
    }
  }

  const COMMAND_NOTES: Record<CommandFailure, Parameters<typeof t>[0]> = {
    offline: "voiceIn.offline",
    busy: "voiceIn.busy",
    oldServer: "voiceIn.oldServer",
    micBlocked: "shell.micBlocked",
    failed: "shell.voiceFailed",
    tooShort: "voiceIn.tooShort",
    noSpeech: "shell.noSpeech",
  };

  /** Back to rest after a voice command ends, however it ends. */
  function commandOver() {
    command = null;
    setListening(false);
    setWorking(false);
    if (stage && (stage.state === "listening" || stage.state === "thinking") && !props.thinking) stage.setState("idle");
  }

  function clearLiveSoon() {
    clearTimeout(liveTimer);
    liveTimer = setTimeout(() => setLiveText(""), 6_000);
  }

  /** Stop the command and her voice (a press while she answers, or the mute button). */
  function stopCommand() {
    command?.cancel();
    player.stop();
    setLiveText("");
    commandOver();
  }

  /** Streaming voice command. False when the socket cannot take it (use the Web Speech path). */
  function startCommand(): boolean {
    if (disposed || socket.oldServer || socket.status !== "open") return false;
    clearTimeout(liveTimer);
    setNote(null);
    setLiveText("");
    // Inside the press: lets iOS and Safari start audio output later.
    void player.unlock().catch(() => {});
    const run = runCommand(
      { socket, player },
      { speak: !muted(), sessionId: chatSessionId(), lang: locale() === "it" || locale() === "en" ? locale() : undefined },
      {
        onProgress: () => {},
        onTranscript: () => {},
        onText: (text) => setLiveText(text),
        onSpoken: () => {
          setListening(false);
          setWorking(true);
          stage?.setState("thinking");
        },
        onStopped: () => setListening(false),
        onFinished: () => {
          commandOver();
          clearLiveSoon();
        },
        onFailure: (why) => {
          setLiveText("");
          commandOver();
          if (why === "oldServer") {
            // Older server: the browser's own speech recognition, then the note.
            void talkLegacy().then(() => setNote(t("voiceIn.oldServer")));
            setNote(t("voiceIn.oldServer"));
          } else setNote(t(COMMAND_NOTES[why]));
        },
      },
    );
    if (typeof run === "string") {
      if (run === "busy") {
        setNote(t("voiceIn.busy"));
        return true;
      }
      return false;
    }
    command = run;
    setListening(true);
    stage?.setState("listening");
    return true;
  }

  function talk() {
    setHint(false);
    if (command) {
      // A second press: while recording it ends the command; while she answers it stops her.
      if (listening()) command.finish();
      else stopCommand();
      return;
    }
    if (listening()) {
      listenCtrl?.abort();
      return;
    }
    if (props.thinking) return;
    if (startCommand()) return;
    void talkLegacy().then(() => {
      if (socket.oldServer) setNote(t("voiceIn.oldServer"));
    });
  }

  async function talkLegacy() {
    if (!voice || !voice.listenSupported) {
      setNote(t("shell.voiceUnavailable"));
      props.onTypeInstead();
      return;
    }
    setNote(null);
    const ctrl = new AbortController();
    listenCtrl = ctrl;
    setListening(true);
    stage?.setState("listening");
    try {
      const text = (await voice.listen({ lang: speechLang(), signal: ctrl.signal, onSpeechStart: () => stage?.nod() })).trim();
      if (text && !disposed) props.onTranscript(text);
    } catch (e) {
      const is = (code: Parameters<StageModule["isVoiceError"]>[1]) => !!mod?.isVoiceError(e, code);
      if (is("not-supported")) {
        setNote(t("shell.voiceUnavailable"));
        props.onTypeInstead();
      } else if (is("permission-denied") || is("audio-capture")) {
        setNote(t("shell.micBlocked"));
      } else if (is("no-speech")) {
        setNote(t("shell.noSpeech"));
      } else if (!is("aborted")) {
        setNote(t("shell.voiceFailed"));
      }
    } finally {
      if (listenCtrl === ctrl) listenCtrl = null;
      setListening(false);
      if (stage && stage.state === "listening") stage.setState(props.thinking ? "thinking" : "idle");
    }
  }

  const activity = (): LucinaActivity =>
    listening() ? "listening" : mode() === "ready" ? lucinaActivity() : props.thinking || working() ? "thinking" : "idle";
  const pillText = () => (liveText() ? liveText().slice(-240) : (note() ?? (hint() ? t("shell.lucinaHint") : null)));

  return (
    <section
      class="stage"
      classList={{ "stage--listening": listening(), "stage--photo": photo() }}
      data-state={activity()}
      aria-label={t("shell.lucina")}
    >
      <Switch>
        <Match when={mode() === "fallback"}>
          <button type="button" class="stage-portrait" onClick={() => void talk()} aria-label={t("shell.lucinaTalk")}>
            <img src="/icon-512.png" alt="" width="512" height="512" />
          </button>
        </Match>
        <Match when={mode() !== "fallback"}>
          <canvas ref={canvas} class="stage-canvas" role="img" aria-label={t("shell.lucinaCanvas")} />
          <Show when={mode() === "loading"}>
            <div class="stage-loading" role="status">
              <span class="holo-loader" aria-hidden="true" />
              <span>{t("shell.lucinaLoading")}</span>
            </div>
          </Show>
        </Match>
      </Switch>
      <div class="stage-hud" aria-hidden="true">
        <span class="stage-hud-dot" />
        <span class="stage-hud-name">{t("shell.lucina")}</span>
        <span class="stage-hud-sep" />
        <span>{outfitChanging() ? t("wardrobe.changing") : t(STATE_LABELS[activity()])}</span>
      </div>
      <div class="stage-controls">
        <button
          type="button"
          class="icon-btn stage-btn stage-btn--mic"
          classList={{ "stage-btn--on": listening() }}
          onClick={() => void talk()}
          disabled={props.thinking && !listening()}
          aria-label={listening() ? t("shell.lucinaStop") : t("shell.lucinaTalk")}
          title={listening() ? t("shell.lucinaStop") : t("shell.lucinaTalk")}
        >
          <MicIcon />
        </button>
        <button
          type="button"
          class="icon-btn stage-btn"
          onClick={toggleMute}
          aria-pressed={muted() ? "true" : "false"}
          aria-label={muted() ? t("shell.lucinaUnmute") : t("shell.lucinaMute")}
          title={muted() ? t("shell.lucinaMutedTitle") : t("shell.lucinaSpeaksTitle")}
        >
          <Show when={muted()} fallback={<VolumeIcon />}>
            <VolumeOffIcon />
          </Show>
        </button>
        <Show when={mode() === "ready"}>
          <button
            type="button"
            class="icon-btn stage-btn"
            classList={{ "stage-btn--busy": studioOpen() }}
            onClick={() => setStudioOpen(!studioOpen())}
            aria-expanded={studioOpen() ? "true" : "false"}
            aria-label={t("studio.open")}
            title={t("studio.title")}
          >
            <SparkleIcon />
          </button>
        </Show>
        <Show when={mode() === "ready"}>
          <button type="button" class="icon-btn stage-btn" onClick={openEditor} aria-label={t("avatarEditor.open")} title={t("avatarEditor.open")}>
            <EditIcon />
          </button>
        </Show>
        <button
          type="button"
          class="icon-btn stage-btn stage-btn--wardrobe"
          classList={{ "stage-btn--busy": outfitChanging() }}
          onClick={openWardrobe}
          aria-label={t("wardrobe.open")}
          title={t("wardrobe.open")}
        >
          <HangerIcon />
        </button>
        <Show when={props.onMinimize}>
          {(minimize) => (
            <button
              type="button"
              class="icon-btn stage-btn"
              onClick={() => {
                stopFace();
                minimize()();
              }}
              aria-label={t("shell.lucinaMinimize")}
              title={t("shell.lucinaMinimize")}
            >
              <CollapseIcon />
            </button>
          )}
        </Show>
        <button
          type="button"
          class="icon-btn stage-btn"
          onClick={() => {
            stopFace();
            props.onCollapse();
          }}
          aria-label={t("shell.lucinaHide")}
          title={t("shell.lucinaHideTitle")}
        >
          <CloseIcon />
        </button>
      </div>
      <Show when={studioOpen() && mode() === "ready" && !photo()}>
        <LucinaStudio
          stage={stageRef}
          extras={extrasRef}
          muted={muted}
          fetchInit={AUTH}
          onClose={() => setStudioOpen(false)}
          onPhoto={() => setPhoto(true)}
          face={isFaceTrackingSupported() ? face : undefined}
          onFace={() => void toggleFace()}
        />
      </Show>
      <Show when={face() !== "off"}>
        <div class="stage-face" role="status">
          <span class="stage-face-dot" aria-hidden="true" />
          <span class="stage-face-label">{face() === "starting" ? t("studio.faceStarting") : t("studio.faceOn")}</span>
          <button type="button" class="studio-chip" onClick={() => tracker?.recenter()} disabled={face() !== "on"}>
            {t("studio.faceRecenter")}
          </button>
          <button type="button" class="studio-chip" onClick={stopFace}>
            {t("studio.faceStop")}
          </button>
        </div>
      </Show>
      <Show when={photo()}>
        <div class="stage-photo-bar">
          <button type="button" class="studio-action" onClick={() => setPhoto(false)}>
            {t("studio.photoDone")}
          </button>
          <button type="button" class="stage-shutter" onClick={() => void takePhoto()} aria-label={t("studio.photoTake")} title={t("studio.photoTake")} />
          <Show when={extrasRef()}>
            <button type="button" class="studio-action" onClick={() => void decoratePhoto()}>
              {t("studio2.decorate")}
            </button>
          </Show>
        </div>
      </Show>
      <Show when={activity() === "listening"}>
        <div class="stage-wave" aria-hidden="true">
          {Array.from({ length: WAVE_BARS }, (_, i) => (
            <span style={{ "animation-delay": `${-((i * 137) % 1000) / 1000}s` }} />
          ))}
        </div>
        <span class="stage-pill" role="status">
          {t("shell.lucinaListening")}
        </span>
      </Show>
      <Show when={activity() !== "listening" && pillText()}>
        {(text) => (
          <span class="stage-pill stage-pill--note" classList={{ "stage-pill--live": !!liveText() }} role="status">
            {text()}
          </span>
        )}
      </Show>
    </section>
  );
}
