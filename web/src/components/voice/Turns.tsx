import { For, Show } from "solid-js";
import type { VoiceTurn } from "../../api/voice";
import { formatBytes, formatClock, formatMs, spanMs, turnBar, type SegmentKey } from "../../lib/voice";
import { relativeTime } from "../../lib/relativeTime";
import { t, type MessageKey } from "../../i18n";
import { Card, EmptyState } from "../page";

// The last turns of a voice device, each with a stacked timing bar from the end of speech:
// STT, Lucina's first text, the first TTS audio, the first audio sent, and the rest of the reply.
// All bars share one scale, so a slow turn is visibly longer than a fast one; the legend under
// each bar carries the numbers, so the bar itself never has to fit a label.

const STAGE_TEXT: Record<SegmentKey, MessageKey> = {
  stt: "voice.stageStt",
  lucina: "voice.stageLucina",
  tts: "voice.stageTts",
  audio: "voice.stageAudio",
  rest: "voice.stageRest",
};

export function Turns(props: { turns: VoiceTurn[] }) {
  // One scale for the list: the longest turn fills the width.
  const scale = () => Math.max(1, ...props.turns.map((turn) => turnBar(turn.spans).end));
  return (
    <Card title={t("voice.turns")} class="vd-card vd-turns">
      <Show when={props.turns.length} fallback={<EmptyState text={t("voice.noTurns")} />}>
        <ol class="vd-turn-list" aria-label={t("voice.turnList")}>
          <For each={props.turns}>{(turn) => <TurnRow turn={turn} scale={scale()} />}</For>
        </ol>
      </Show>
    </Card>
  );
}

function TurnRow(props: { turn: VoiceTurn; scale: number }) {
  const turn = () => props.turn;
  const bar = () => turnBar(turn().spans);
  const first = () => spanMs(turn().spans?.first_audio);
  const total = () => spanMs(turn().spans?.total);
  const deviceFirst = () => spanMs(turn().device_first_audio_ms);
  const ack = () => spanMs(turn().spans?.ack);
  const intent = () => (typeof turn().intent === "string" && turn().intent ? turn().intent! : null);
  const detail = () =>
    t("voice.routeDetail", {
      intent: intent() ?? "",
      confidence: typeof turn().confidence === "number" ? turn().confidence!.toFixed(2) : "-",
      jev: spanMs(turn().jev_ms) !== null ? formatMs(turn().jev_ms!) : "-",
      source: turn().source || "-",
    });
  const parts = () => bar().segments.map((s) => `${t(STAGE_TEXT[s.key])} ${formatMs(s.ms)}`).join(", ");
  return (
    <li class="vd-turn" data-ok={turn().ok ? "true" : "false"}>
      <div class="vd-turn-head">
        <time class="vd-turn-time" dateTime={turn().at}>{formatClock(turn().at)}</time>
        <span class="pg-muted vd-turn-ago">{relativeTime(turn().at)}</span>
        <span class="vd-badge" data-kind={turn().ok ? "ok" : "error"}>
          {turn().ok ? t("voice.turnOk") : turn().err ? t("voice.turnFailedCode", { code: turn().err! }) : t("voice.turnFailed")}
        </span>
        <Show when={intent()}>
          <span class="vd-intent" title={detail()}>{intent()}</span>
        </Show>
        <Show when={turn().fast === true}><span class="vd-badge" data-kind="fast">{t("voice.fastBadge")}</span></Show>
      </div>

      <Show when={bar().segments.length} fallback={<p class="pg-muted vd-notimings">{t("voice.noTimings")}</p>}>
        <div class="vd-bar-wrap">
          <div class="vd-bar" role="img" aria-label={t("voice.timingBar", { parts: parts() })}>
            <For each={bar().segments}>
              {(s) => <span class="vd-seg" data-stage={s.key} style={{ width: `${(s.ms / props.scale) * 100}%` }} />}
            </For>
          </div>
          <Show when={ack() !== null}>
            <span class="vd-ack-mark" title={t("voice.ackAfter", { time: formatMs(ack()!) })} style={{ left: `${Math.min(100, (ack()! / props.scale) * 100)}%` }} />
          </Show>
        </div>
        <ul class="vd-legend">
          <For each={bar().segments}>
            {(s) => (
              <li class="vd-leg" data-stage={s.key}>
                <span class="vd-swatch" data-stage={s.key} aria-hidden="true" />
                <span class="vd-leg-name">{t(STAGE_TEXT[s.key])}</span>
                <span class="vd-leg-ms">{formatMs(s.ms)}</span>
              </li>
            )}
          </For>
        </ul>
      </Show>

      <p class="vd-turn-meta">
        <Show when={first() !== null}><span class="vd-first">{t("voice.speaksAfter", { time: formatMs(first()!) })}</span></Show>
        <Show when={ack() !== null}><span class="vd-ack">{t("voice.ackAfter", { time: formatMs(ack()!) })}</span></Show>
        <Show when={total() !== null}><span class="vd-total">{t("voice.total", { time: formatMs(total()!) })}</span></Show>
        <Show when={deviceFirst() !== null}><span class="vd-device">{t("voice.deviceFirstAudio", { time: formatMs(deviceFirst()!) })}</span></Show>
        <Show when={typeof turn().speech_ms === "number"}><span>{t("voice.youSpoke", { time: formatMs(turn().speech_ms!) })}</span></Show>
        <Show when={typeof turn().transcript_chars === "number"}><span>{t("voice.chars", { count: turn().transcript_chars! })}</span></Show>
        <Show when={typeof turn().up_bytes === "number"}><span>{t("voice.sent", { bytes: formatBytes(turn().up_bytes!) })}</span></Show>
        <Show when={typeof turn().down_bytes === "number"}><span>{t("voice.received", { bytes: formatBytes(turn().down_bytes!) })}</span></Show>
      </p>
    </li>
  );
}
