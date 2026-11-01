import { A, useParams } from "@solidjs/router";
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { ApiError, toApiError } from "../api/client";
import { voiceApi, type VoiceDeviceSummary, type VoiceStatus } from "../api/voice";
import { OnlineWord, LiveCard, NetworkCard, PowerCard, Signal, SpecsCard } from "../components/voice/Cards";
import { EventsTab } from "../components/voice/Events";
import { Turns } from "../components/voice/Turns";
import { BackIcon, RefreshIcon } from "../components/Icons";
import { Card, EmptyState, ErrorState, ListRow, LoadingState, Page, PageHeader, RowList, SectionNav } from "../components/page";
import { t } from "../i18n";
import { relativeTime } from "../lib/relativeTime";
import "../styles/voice.css";

// The Voice device page (Plugins → Voice devices): the LucinaVoice ESP32 speaker's status and
// telemetry from the device API (docs/lucinavoice-protocol.md section 5). /voice lists the
// devices; /voice/<id> shows one (specs, live, network, power and the last turns) and
// /voice/<id>/events its event log. The status refreshes every 10 s while the tab is visible.

export const REFRESH_MS = 10_000;
export const BASE = "/voice";

/** Load now, then every REFRESH_MS while the tab is visible, and again when it comes back. */
function usePoll(load: (fresh: boolean) => Promise<void>) {
  onMount(() => {
    void load(false);
    const timer = window.setInterval(() => {
      if (!document.hidden) void load(false);
    }, REFRESH_MS);
    const onVisible = () => {
      if (!document.hidden) void load(false);
    };
    document.addEventListener("visibilitychange", onVisible);
    window.addEventListener("focus", onVisible);
    onCleanup(() => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
      window.removeEventListener("focus", onVisible);
    });
  });
}

/** One loader with a request counter: a slow answer never overwrites a newer one. */
function createLoader<T>(fetcher: () => Promise<T>) {
  const [data, setData] = createSignal<T | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [loading, setLoading] = createSignal(false);
  let seq = 0;
  onCleanup(() => ++seq);
  const load = async (fresh = false) => {
    if (loading() || (!fresh && document.hidden)) return;
    const mine = ++seq;
    setLoading(true);
    try {
      const next = await fetcher();
      if (mine !== seq) return;
      setData(() => next);
      setError(null);
    } catch (e) {
      if (mine !== seq) return;
      setError(toApiError(e));
    } finally {
      if (mine === seq) setLoading(false);
    }
  };
  return { data, error, loading, load };
}

function RefreshButton(props: { loading: boolean; onClick: () => void }) {
  return (
    <button
      type="button"
      class="pg-icon-btn vd-refresh"
      classList={{ "vd-refresh--busy": props.loading }}
      aria-label={t("voice.refresh")}
      aria-busy={props.loading}
      disabled={props.loading}
      onClick={() => props.onClick()}
    >
      <RefreshIcon />
    </button>
  );
}

export default function VoiceView() {
  const params = useParams<{ id?: string; tab?: string }>();
  return (
    <Show when={params.id} keyed fallback={<DeviceList />}>
      {(id) => <DeviceView id={id} />}
    </Show>
  );
}

// ---- The list ------------------------------------------------------------------------------

function DeviceList() {
  const { data, error, loading, load } = createLoader(() => voiceApi.devices());
  usePoll(load);
  return (
    <Page class="vd-page" label={t("voice.title")}>
      <A href="/plugins" class="pg-link pg-back">
        <BackIcon /> {t("voice.back")}
      </A>
      <PageHeader title={t("voice.title")} action={<RefreshButton loading={loading()} onClick={() => void load(true)} />} />
      <Show
        when={data()}
        fallback={
          <Show when={error()} fallback={<LoadingState label={t("voice.loading")} />}>
            <ErrorState message={t("voice.loadError")} retryLabel={t("voice.retry")} onRetry={() => void load(true)} />
          </Show>
        }
      >
        {(devices) => (
          <>
            <Show when={error()}>
              <p class="vd-banner" role="alert">{t("voice.refreshError", { error: error()!.message })}</p>
            </Show>
            <Show when={devices().length} fallback={<Card><EmptyState title={t("voice.noDevices")} text={t("voice.noDevicesText")} /></Card>}>
              <Card list label={t("voice.devices")} class="vd-devices">
                <RowList label={t("voice.devices")}>
                  <For each={devices()}>{(d) => <DeviceRow device={d} />}</For>
                </RowList>
              </Card>
            </Show>
          </>
        )}
      </Show>
    </Page>
  );
}

function DeviceRow(props: { device: VoiceDeviceSummary }) {
  const d = () => props.device;
  return (
    <ListRow
      title={d().name || d().id}
      href={`${BASE}/${encodeURIComponent(d().id)}`}
      sub={
        <>
          <OnlineWord online={d().online} />
          {" · "}
          {d().last_seen && !d().online ? t("voice.lastSeen", { time: relativeTime(d().last_seen!) }) : d().fw ? t("voice.firmware", { fw: d().fw! }) : ""}
        </>
      }
      meta={
        <>
          <Show when={typeof d().rssi === "number"}><Signal rssi={d().rssi!} /></Show>
          <Show when={typeof d().est_mw === "number"}>
            {typeof d().rssi === "number" ? " · " : ""}
            {t("voice.powerValue", { mw: Math.round(d().est_mw!) })}
          </Show>
        </>
      }
    />
  );
}

// ---- One device ----------------------------------------------------------------------------

function DeviceView(props: { id: string }) {
  const params = useParams<{ id?: string; tab?: string }>();
  const { data, error, loading, load } = createLoader(() => voiceApi.status(props.id));
  const [reload, setReload] = createSignal(0);
  usePoll(load);

  const tab = () => (params.tab === "events" ? "events" : "overview");
  const base = `${BASE}/${encodeURIComponent(props.id)}`;
  const refresh = () => {
    void load(true);
    setReload((n) => n + 1);
  };

  const navItems = () => [
    { label: t("voice.overview"), href: base, current: tab() === "overview" },
    { label: t("voice.events"), href: `${base}/events`, current: tab() === "events" },
  ];

  const context = (s: VoiceStatus) => (
    <>
      <OnlineWord online={s.online} />
      {" · "}
      {s.online && s.connected_since
        ? t("voice.onlineSince", { time: relativeTime(s.connected_since) })
        : s.last_seen
          ? t("voice.lastSeen", { time: relativeTime(s.last_seen) })
          : t("voice.neverSeen")}
      <Show when={s.specs.fw}>{" · "}{t("voice.firmware", { fw: s.specs.fw! })}</Show>
    </>
  );

  return (
    <Page class="vd-page" label={t("voice.title")}>
      <A href={BASE} class="pg-link pg-back">
        <BackIcon /> {t("voice.backToList")}
      </A>
      <PageHeader
        title={data()?.name ?? props.id}
        context={<Show when={data()} fallback={t("voice.loading")}>{(s) => context(s())}</Show>}
        nav={<SectionNav label={t("voice.sections")} items={navItems()} />}
        action={<RefreshButton loading={loading()} onClick={refresh} />}
      />

      <Show
        when={data()}
        fallback={
          <Show when={error()} fallback={<LoadingState label={t("voice.loading")} />}>
            <ErrorState
              message={error()!.status === 404 ? t("voice.notFound") : t("voice.deviceError")}
              retryLabel={t("voice.retry")}
              onRetry={() => void load(true)}
            />
          </Show>
        }
      >
        {(s) => (
          <>
            <Show when={error()}>
              <p class="vd-banner" role="alert">{t("voice.refreshError", { error: error()!.message })}</p>
            </Show>
            <Show when={!s().online}>
              <p class="vd-banner vd-banner--offline" role="status">{t("voice.offlineNote")}</p>
            </Show>
            <Show when={tab() === "overview"} fallback={<EventsTab id={props.id} reload={reload()} />}>
              <div class="vd-grid">
                <LiveCard status={s()} />
                <SpecsCard specs={s().specs} />
                <NetworkCard network={s().network} />
                <PowerCard power={s().power} ina={s().specs.ina} />
              </div>
              <Turns turns={s().turns} />
            </Show>
          </>
        )}
      </Show>
    </Page>
  );
}
