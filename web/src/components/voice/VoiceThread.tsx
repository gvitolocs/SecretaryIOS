import { createSignal, For, Show } from "solid-js";
import { toApiError } from "../../api/client";
import type { VoiceMeta, VoiceMsKey } from "../../api/timeline";
import { voiceLearnApi, type LearnSuggestion } from "../../api/voiceLearn";
import { t, type MessageKey } from "../../i18n";
import { formatMs } from "../../lib/voice";
import type { ChatMessage } from "../../state/chat";
import { LayerChips } from "./LayerChips";
import "../../styles/voice.css";
import "../../styles/voice-thread.css";

// What sits under a message of the "Lucina Voice" chat (the one thread all LucinaVoice speakers share,
// docs/lucinavoice-protocol.md section 7). The thread is read-only; these are its two exceptions:
//   - a Lucina reply gets a "Timing" line that opens into the turn's stages and the Jev layers that decided it
//     (meta.voice: ms, route, layers);
//   - your own message gets "Edit transcript": type what you really said, ask Voice Learn for a suggestion
//     (POST /v1/voice/learn/suggest) and add it (POST /v1/voice/learn). History is not rewritten.

const STAGES: [VoiceMsKey, MessageKey][] = [
  ["stt", "voice.stageStt"],
  ["lucina_first", "voice.stageLucina"],
  ["tts_first", "voice.stageTts"],
  ["first_audio", "voice.stageAudio"],
];

export function VoiceExtras(props: { message: ChatMessage }) {
  const voice = () => props.message.meta?.voice;
  const device = () => props.message.meta?.deviceName;
  return (
    <Show when={props.message.channel === "voice"}>
      <div class="vt-extras" data-role={props.message.role}>
        <Show when={props.message.role === "lucina"}>
          <Timing voice={voice()} device={device()} />
        </Show>
        <Show when={props.message.role === "user"}>
          <EditTranscript text={props.message.text} />
        </Show>
      </div>
    </Show>
  );
}

function Timing(props: { voice: VoiceMeta | undefined; device: string | undefined }) {
  const [open, setOpen] = createSignal(false);
  const v = () => props.voice;
  // Lucina keeps the speech-to-text time in ms.stt, the rest of the turn in its route and layers.
  const stages = () => STAGES.flatMap(([k, label]) => (typeof v()?.ms?.[k] === "number" ? [{ label: t(label), ms: v()!.ms![k]! }] : []));
  const total = () => v()?.ms?.total;
  const any = () => stages().length > 0 || total() !== undefined || v()?.sttMs !== undefined || !!v()?.layers?.length || !!v()?.route;
  return (
    <Show when={any() || props.device}>
      <div class="vt-timing">
        <div class="vt-actions">
          <Show when={any()}>
            <button type="button" class="vt-link" aria-expanded={open()} onClick={() => setOpen(!open())}>
              {open() ? t("voiceLearn.hideTiming") : t("voiceLearn.timing")}
              <Show when={!open() && total() !== undefined}>{` · ${formatMs(total()!)}`}</Show>
            </button>
          </Show>
          <Show when={props.device}>
            <span>{t("voiceLearn.heardBy", { device: props.device! })}</span>
          </Show>
        </div>
        <Show when={open()}>
          <ul class="vt-timing-list" aria-label={t("voiceLearn.timingList")}>
            <For each={stages()}>
              {(s) => (
                <li>
                  {s.label} <b>{formatMs(s.ms)}</b>
                </li>
              )}
            </For>
            <Show when={stages().length === 0 && v()?.sttMs !== undefined}>
              <li>
                <b>{t("voiceLearn.sttOnly", { time: formatMs(v()!.sttMs!) })}</b>
              </li>
            </Show>
            <Show when={total() !== undefined}>
              <li>
                <b>{t("voice.total", { time: formatMs(total()!) })}</b>
              </li>
            </Show>
            <Show when={v()?.ms?.ack !== undefined}>
              <li>{t("voice.ackAfter", { time: formatMs(v()!.ms!.ack!) })}</li>
            </Show>
            <Show when={v()?.route?.intent}>
              {(intent) => (
                <li>
                  {t("voiceLearn.routedAs", { intent: intent() })}
                  <Show when={v()?.route?.fast}>{` (${t("voiceLearn.fastRoute")})`}</Show>
                </li>
              )}
            </Show>
            <Show when={v()?.route?.jev_ms !== undefined}>
              <li>{t("voiceLearn.jevTime", { time: formatMs(v()!.route!.jev_ms!) })}</li>
            </Show>
            <Show when={v()?.corrections?.length}>
              <li>{t("voice.sttCorrections", { count: v()!.corrections!.length })}</li>
            </Show>
            <Show when={v()?.ambiguous}>
              <li>{t("voice.sttAmbiguous", { count: v()!.ambiguous! })}</li>
            </Show>
          </ul>
          <Show when={!any()}>
            <p>{t("voiceLearn.timingNone")}</p>
          </Show>
        </Show>
        {/* The layers that fired are the point of the line, so the chips stay visible when it is closed. */}
        <LayerChips layers={v()?.layers} />
      </div>
    </Show>
  );
}

type Phase = "closed" | "editing" | "suggesting";

let editSeq = 0;

function EditTranscript(props: { text: string }) {
  const field = `vt-corrected-${++editSeq}`;
  const [phase, setPhase] = createSignal<Phase>("closed");
  const [corrected, setCorrected] = createSignal(props.text);
  const [notice, setNotice] = createSignal("");
  const [error, setError] = createSignal("");
  const [suggestions, setSuggestions] = createSignal<LearnSuggestion[] | null>(null);

  function open() {
    setCorrected(props.text);
    setNotice("");
    setError("");
    setSuggestions(null);
    setPhase("editing");
  }

  async function suggest(e: Event) {
    e.preventDefault();
    const fixed = corrected().trim();
    setNotice("");
    setError("");
    setSuggestions(null);
    if (!fixed || fixed === props.text.trim()) {
      setNotice(t("voiceLearn.editSame"));
      return;
    }
    setPhase("suggesting");
    try {
      const found = await voiceLearnApi.suggest(props.text, fixed);
      setSuggestions(found);
      if (!found.length) setNotice(t("voiceLearn.editNone"));
    } catch (err) {
      setError(t("voiceLearn.editFailed", { error: toApiError(err).message }));
    } finally {
      setPhase("editing");
    }
  }

  return (
    <Show
      when={phase() !== "closed"}
      fallback={
        <button type="button" class="vt-link" onClick={open}>
          {t("voiceLearn.editTranscript")}
        </button>
      }
    >
      <div class="vt-form" role="group" aria-label={t("voiceLearn.editTitle")}>
      <form class="vt-edit" onSubmit={(e) => void suggest(e)}>
        <label for={field}>{t("voiceLearn.editLabel")}</label>
        <textarea
          id={field}
          rows="3"
          maxlength="2000"
          value={corrected()}
          onInput={(e) => setCorrected(e.currentTarget.value)}
          disabled={phase() === "suggesting"}
        />
        <div class="vt-form-actions">
          <button type="submit" class="btn btn--small" disabled={phase() === "suggesting"}>
            {phase() === "suggesting" ? t("voiceLearn.editSuggesting") : t("voiceLearn.editSuggest")}
          </button>
          <button type="button" class="btn btn--small" onClick={() => setPhase("closed")}>
            {t("voiceLearn.cancel")}
          </button>
        </div>
        <Show when={notice()}>
          <p role="status">{notice()}</p>
        </Show>
        <Show when={error()}>
          <p class="vt-error" role="alert">
            {error()}
          </p>
        </Show>
      </form>
        <For each={suggestions() ?? []}>{(s) => <Suggestion suggestion={s} />}</For>
      </div>
    </Show>
  );
}

function Suggestion(props: { suggestion: LearnSuggestion }) {
  const [meaning, setMeaning] = createSignal(props.suggestion.meaning ?? "");
  const [state, setState] = createSignal<"idle" | "saving" | "added">("idle");
  const [error, setError] = createSignal("");
  const s = () => props.suggestion;

  async function add() {
    setState("saving");
    setError("");
    try {
      await voiceLearnApi.create({
        word: s().word,
        ...(s().soundsLike.length ? { soundsLike: s().soundsLike } : {}),
        ...(meaning().trim() ? { meaning: meaning().trim() } : {}),
        ...(s().lang ? { lang: s().lang } : {}),
      });
      setState("added");
    } catch (err) {
      setState("idle");
      setError(t("voiceLearn.suggestionAddFailed", { error: toApiError(err).message }));
    }
  }

  return (
    <div class="vt-suggest" role="group" aria-label={t("voiceLearn.suggestion")}>
      <p>
        <span class="vt-suggest-word">{s().word}</span>
        <Show when={s().soundsLike.length}>{` · ${t("voiceLearn.soundsLikeValue", { list: s().soundsLike.join(", ") })}`}</Show>
      </p>
      <Show when={state() !== "added"} fallback={<p role="status">{t("voiceLearn.suggestionAdded")}</p>}>
        <label for={`vt-meaning-${s().word}`}>{t("voiceLearn.meaning")}</label>
        <input
          id={`vt-meaning-${s().word}`}
          value={meaning()}
          maxlength="100"
          placeholder={t("voiceLearn.meaningHint")}
          onInput={(e) => setMeaning(e.currentTarget.value)}
          disabled={state() === "saving"}
        />
        <div class="vt-form-actions">
          <button type="button" class="btn btn--small" disabled={state() === "saving"} onClick={() => void add()}>
            {t("voiceLearn.suggestionAdd")}
          </button>
        </div>
      </Show>
      <Show when={error()}>
        <p class="vt-error" role="alert">
          {error()}
        </p>
      </Show>
    </div>
  );
}
