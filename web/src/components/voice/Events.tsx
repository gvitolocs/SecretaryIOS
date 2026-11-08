import { createEffect, createSignal, For, on, onMount, Show } from "solid-js";
import { toApiError } from "../../api/client";
import { VOICE_KINDS, voiceApi, type VoiceEvent, type VoiceKind } from "../../api/voice";
import { formatBytes, formatClock, formatMs, formatUptime } from "../../lib/voice";
import { t, type MessageKey } from "../../i18n";
import { Button, Card, EmptyState, ErrorState, LoadingState } from "../page";

// The Events tab: what the device reported, newest first, filtered on the server by kind
// (boot, wifi, link, turn, stats, error). "Load more" asks for the next older page with the
// `next` cursor of the last one.

const PAGE = 100;

export const KIND_TEXT: Record<VoiceKind, MessageKey> = {
  boot: "voice.kindBoot",
  wifi: "voice.kindWifi",
  link: "voice.kindLink",
  turn: "voice.kindTurn",
  stats: "voice.kindStats",
  error: "voice.kindError",
};

const str = (v: unknown): string | undefined => (typeof v === "string" && v ? v : typeof v === "number" ? String(v) : undefined);
const numOf = (v: unknown): number | undefined => (typeof v === "number" && Number.isFinite(v) ? v : undefined);

/** One line for an event; a kind this page doesn't know shows its fields as key=value. */
export function eventSummary(e: VoiceEvent): string {
  switch (e.k) {
    case "boot":
      return numOf(e.seq) !== undefined ? t("voice.evBoot", { reset: str(e.reset) ?? "?", seq: numOf(e.seq)! }) : t("voice.evBootNoSeq", { reset: str(e.reset) ?? "?" });
    case "wifi":
      if (e.up) return numOf(e.rssi) !== undefined ? t("voice.evWifiUpRssi", { rssi: numOf(e.rssi)! }) : t("voice.evWifiUp");
      return str(e.reason) ? t("voice.evWifiDownReason", { reason: str(e.reason)! }) : t("voice.evWifiDown");
    case "link":
      if (e.up) return numOf(e.ms) !== undefined ? t("voice.evLinkUpMs", { time: formatMs(numOf(e.ms)!) }) : t("voice.evLinkUp");
      return str(e.err) ? t("voice.evLinkDownErr", { err: str(e.err)! }) : t("voice.evLinkDown");
    case "turn": {
      const mode = str(e.mode) ?? "?";
      if (!e.ok) return t("voice.evTurnErr", { mode, err: str(e.err) ?? "?" });
      const total = formatMs(numOf(e.total_ms) ?? 0);
      return numOf(e.first_audio_ms) !== undefined
        ? t("voice.evTurnOkAudio", { mode, audio: formatMs(numOf(e.first_audio_ms)!), time: total })
        : t("voice.evTurnOk", { mode, time: total });
    }
    case "stats": {
      const args = { heap: formatBytes(numOf(e.heap_free) ?? 0), uptime: formatUptime(numOf(e.uptime_ms) ?? 0) };
      return numOf(e.rssi) !== undefined ? t("voice.evStatsRssi", { ...args, rssi: numOf(e.rssi)! }) : t("voice.evStats", args);
    }
    case "error":
      return str(e.detail) ? t("voice.evErrorDetail", { what: str(e.what) ?? "?", detail: str(e.detail)! }) : t("voice.evError", { what: str(e.what) ?? "?" });
    default:
      return Object.entries(e)
        .filter(([k]) => k !== "at" && k !== "k")
        .map(([k, v]) => `${k}=${typeof v === "object" ? JSON.stringify(v) : String(v)}`)
        .join(" ");
  }
}

const isKind = (k: string): k is VoiceKind => (VOICE_KINDS as readonly string[]).includes(k);

/** `reload` changes when the page's Refresh button is pressed: the list starts again from the newest event. */
export function EventsTab(props: { id: string; reload?: number }) {
  const [kind, setKind] = createSignal<VoiceKind | null>(null);
  const [events, setEvents] = createSignal<VoiceEvent[]>([]);
  const [next, setNext] = createSignal<string | null>(null);
  const [state, setState] = createSignal<"loading" | "ready" | "error" | "more">("loading");
  const [error, setError] = createSignal("");
  let seq = 0;

  const load = async (more = false) => {
    const mine = ++seq;
    setState(more ? "more" : "loading");
    try {
      const page = await voiceApi.events(props.id, { kind: kind(), limit: PAGE, before: more ? next() : null });
      if (mine !== seq) return;
      setEvents(more ? [...events(), ...page.events] : page.events);
      setNext(page.next ?? null);
      setError("");
      setState("ready");
    } catch (e) {
      if (mine !== seq) return;
      setError(toApiError(e).message);
      // A failed "load more" keeps what is on screen and says so; a failed first load shows the error card.
      setState(more ? "ready" : "error");
    }
  };

  onMount(() => void load());
  createEffect(on(() => props.reload, () => void load(), { defer: true }));

  const pick = (k: VoiceKind | null) => {
    if (k === kind() && state() !== "error") return;
    setKind(k);
    setEvents([]);
    setNext(null);
    void load();
  };

  const kindLabel = (k: VoiceKind | null) => (k ? t(KIND_TEXT[k]) : t("voice.kindAll"));

  return (
    <Card title={t("voice.eventsTitle")} class="vd-card vd-events">
      <div class="vd-chips" role="group" aria-label={t("voice.filter")}>
        <For each={[null, ...VOICE_KINDS] as (VoiceKind | null)[]}>
          {(k) => (
            <button type="button" class="vd-chip" data-kind={k ?? "all"} aria-pressed={kind() === k} onClick={() => pick(k)}>
              {kindLabel(k)}
            </button>
          )}
        </For>
      </div>

      <Show when={state() !== "loading" || events().length} fallback={<LoadingState label={t("voice.loading")} />}>
        <Show when={state() !== "error"} fallback={<ErrorState message={error() || t("voice.loadError")} retryLabel={t("voice.retry")} onRetry={() => void load()} />}>
          <Show when={events().length} fallback={<EmptyState text={kind() ? t("voice.noEventsKind", { kind: kindLabel(kind()) }) : t("voice.noEvents")} />}>
            <ul class="vd-event-list" aria-label={t("voice.eventList")}>
              <For each={events()}>
                {(e) => (
                  <li class="vd-event" data-kind={e.k} data-ok={e.ok === false || e.k === "error" ? "false" : undefined}>
                    <time class="vd-event-time" dateTime={e.at}>{formatClock(e.at)}</time>
                    <span class="vd-kind" data-kind={e.k}>{isKind(e.k) ? t(KIND_TEXT[e.k]) : e.k}</span>
                    <span class="vd-event-text">{eventSummary(e)}</span>
                  </li>
                )}
              </For>
            </ul>
          </Show>
          <Show when={error() && state() === "ready"}>
            <p class="vd-banner" role="alert">{t("voice.refreshError", { error: error() })}</p>
          </Show>
          <Show when={next()}>
            <div class="vd-more">
              <Button disabled={state() === "more"} aria-busy={state() === "more"} onClick={() => void load(true)}>
                {t("voice.loadMore")}
              </Button>
            </div>
          </Show>
        </Show>
      </Show>
    </Card>
  );
}
