import type { Framing, LucinaStage, Voice } from "@secretary/lucina-stage";
import { createEffect, createSignal, Match, on, onCleanup, onMount, Show, Switch } from "solid-js";
import { speechLang, t } from "../i18n";
import { readString, writeString } from "../lib/storage";
import { loadStage, type StageModule } from "../lib/stageLoader";
import { onLucinaReply } from "../state/chat";
import { CloseIcon, CollapseIcon, MicIcon, VolumeIcon, VolumeOffIcon } from "./Icons";

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
const AUTH: RequestInit = { credentials: "include", headers: { "X-Secretary-CSRF": "1" } };

/** Muted unless the owner has unmuted before: sound must never come as a surprise. */
export function storedMuted(): boolean {
  return readString("local", MUTE_KEY) !== "0";
}

type Mode = "loading" | "ready" | "fallback";

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
  let canvas: HTMLCanvasElement | undefined;
  let mod: StageModule | null = null;
  let stage: LucinaStage | null = null;
  let voice: Voice | null = null;
  let listenCtrl: AbortController | null = null;
  let ttsCtrl: AbortController | null = null;
  let hintTimer: ReturnType<typeof setTimeout> | undefined;
  let disposed = false;

  function showHintOnce() {
    if (readString("local", HINT_KEY)) return;
    writeString("local", HINT_KEY, "1");
    setHint(true);
    hintTimer = setTimeout(() => setHint(false), HINT_MS);
  }

  onMount(async () => {
    try {
      mod = await loadStage();
      if (disposed) return;
      voice = mod.createVoice({ ttsUrl: TTS_URL, fetchInit: AUTH, lang: speechLang() });
      if (!canvas) throw new Error("canvas missing");
      const s = await mod.createLucinaStage(canvas, {
        modelUrl: MODEL_URL,
        manifestUrl: MANIFEST_URL,
        clipsUrl: CLIPS_URL,
        clipsMetaUrl: CLIPS_META_URL,
        fetchInit: AUTH,
        framing: props.framing,
        muteStorageKey: MUTE_KEY,
        muted: true,
      });
      if (disposed) {
        s.dispose();
        return;
      }
      stage = s;
      s.setMuted(muted());
      s.onTap(() => void talk());
      if (props.thinking) s.setState("thinking");
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
    clearTimeout(hintTimer);
    listenCtrl?.abort();
    ttsCtrl?.abort();
    stage?.dispose();
    stage = null;
  });

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

  function toggleMute() {
    const next = !muted();
    setMuted(next);
    writeString("local", MUTE_KEY, next ? "1" : "0");
    stage?.setMuted(next);
    if (next) {
      ttsCtrl?.abort();
      stage?.stopSpeaking();
    } else {
      // Inside the click: lets iOS start audio output later.
      void stage?.unlockAudio().catch(() => {});
    }
  }

  async function talk() {
    setHint(false);
    if (listening()) {
      listenCtrl?.abort();
      return;
    }
    if (props.thinking) return;
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

  return (
    <section class="stage" classList={{ "stage--listening": listening() }} aria-label={t("shell.lucina")}>
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
              <span class="spinner" aria-hidden="true" />
              <span>{t("shell.lucinaLoading")}</span>
            </div>
          </Show>
        </Match>
      </Switch>
      <div class="stage-controls">
        <button
          type="button"
          class="icon-btn stage-btn"
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
        <Show when={props.onMinimize}>
          {(minimize) => (
            <button
              type="button"
              class="icon-btn stage-btn"
              onClick={() => minimize()()}
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
          onClick={() => props.onCollapse()}
          aria-label={t("shell.lucinaHide")}
          title={t("shell.lucinaHideTitle")}
        >
          <CloseIcon />
        </button>
      </div>
      <Show when={listening()}>
        <span class="stage-pill" role="status">
          {t("shell.lucinaListening")}
        </span>
      </Show>
      <Show when={!listening() && (note() ?? (hint() ? t("shell.lucinaHint") : null))}>
        {(text) => (
          <span class="stage-pill stage-pill--note" role="status">
            {text()}
          </span>
        )}
      </Show>
    </section>
  );
}
