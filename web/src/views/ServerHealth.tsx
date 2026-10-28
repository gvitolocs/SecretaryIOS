import { A } from "@solidjs/router";
import {
  createMemo,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
} from "solid-js";
import { toApiError } from "../api/client";
import {
  healthyByGroup,
  needsAttention,
  systemHealthApi,
  type CheckGroup,
  type HealthEvent,
  type HealthState,
  type HostHealth,
  type SystemHealth,
} from "../api/systemHealth";
import { relativeTime } from "../components/NotificationBell";
import { BackIcon, RefreshIcon } from "../components/Icons";
import {
  Page,
  PageHeader,
  PageGrid,
  Card,
  RowList,
  ListRow,
  Button,
  EmptyState,
  LoadingState,
  ErrorState,
  useWide,
} from "../components/page";
import { locale, t } from "../i18n";
import "../styles/server-health.css";

// Server health (Plugins → Server health): each host's latest snapshot from its sentinel
// (state, metrics, the checks that need attention, the healthy ones collapsed by group) and the
// state changes of the last 30 days. Refreshes every minute while the page is visible.

export const REFRESH_MS = 60_000;
const HISTORY_PAGE = 50;

const stateLabel = (s: HealthState) => t(`serverHealth.${s}`);
const groupLabel = (g: CheckGroup) =>
  g === "Other" ? t("serverHealth.groupOther") : g;

/** "12 min", "3 h", "2 d": how long ago `iso` was, without "ago". */
export function duration(
  iso: string,
  now = Date.now(),
  loc = locale(),
): string {
  const min = Math.max(0, Math.floor((now - Date.parse(iso)) / 60_000));
  const [value, unit] =
    min < 60
      ? [min, "minute"]
      : min < 48 * 60
        ? [Math.floor(min / 60), "hour"]
        : [Math.floor(min / 1440), "day"];
  try {
    return new Intl.NumberFormat(loc, {
      style: "unit",
      unit,
      unitDisplay: "short",
    }).format(value);
  } catch {
    return `${value} ${unit}`;
  }
}

/** "10:30" today, "27 Oct, 10:30" otherwise. */
function clock(iso: string, loc = locale()): string {
  const d = new Date(iso);
  const today = new Date().toDateString() === d.toDateString();
  const opts: Intl.DateTimeFormatOptions = today
    ? { hour: "2-digit", minute: "2-digit" }
    : { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" };
  return new Intl.DateTimeFormat(loc, opts).format(d);
}

const num = (n: number, digits = 0) =>
  new Intl.NumberFormat(locale(), { maximumFractionDigits: digits }).format(n);

/** The host's state as shown: its overall state, or "stale" when it stopped reporting. */
export function hostState(h: HostHealth): HealthState | "stale" {
  return h.stale ? "stale" : h.snapshot.overall;
}

function HostTiles(props: { host: HostHealth }) {
  const m = () => props.host.snapshot.metrics;
  return (
    <div class="sh-tiles">
      <For each={m().disks}>
        {(d) => (
          <div
            class="sh-tile"
            data-level={
              d.used_pct >= 95 ? "crit" : d.used_pct >= 85 ? "warn" : "ok"
            }
          >
            <span class="sh-tile-label">
              {t("serverHealth.diskFree", { mount: d.mount })}
            </span>
            <strong class="sh-tile-value">{num(d.free_gb)} GB</strong>
            <span class="sh-tile-sub">
              {t("serverHealth.diskValue", {
                free: num(d.free_gb),
                used: num(d.used_pct, 1),
              })}
            </span>
          </div>
        )}
      </For>
      <div class="sh-tile">
        <span class="sh-tile-label">{t("serverHealth.cpuTemp")}</span>
        <strong class="sh-tile-value">
          {m().cpu_temp_c == null
            ? t("serverHealth.unknown")
            : `${num(m().cpu_temp_c as number, 1)} °C`}
        </strong>
      </div>
      <div class="sh-tile">
        <span class="sh-tile-label">{t("serverHealth.memory")}</span>
        <strong class="sh-tile-value">{num(m().mem_used_pct)}%</strong>
        <span class="sh-tile-sub">
          {t("serverHealth.memoryValue", { used: num(m().mem_used_pct, 1) })}
        </span>
      </div>
      <div class="sh-tile">
        <span class="sh-tile-label">{t("serverHealth.load")}</span>
        <strong class="sh-tile-value">{num(m().load[0], 2)}</strong>
        <span class="sh-tile-sub">
          {m()
            .load.map((l) => num(l, 2))
            .join(" · ")}
        </span>
      </div>
    </div>
  );
}

function HostDetail(props: { host: HostHealth; named: boolean }) {
  const checks = () => props.host.snapshot.checks;
  const attention = createMemo(() => needsAttention(checks()));
  const healthy = createMemo(() => healthyByGroup(checks()));
  const healthyCount = () => healthy().reduce((n, g) => n + g.checks.length, 0);
  const suffix = () => (props.named ? ` · ${props.host.host}` : "");
  return (
    <section class="sh-host" data-host={props.host.host}>
      <h2>
        {t("serverHealth.metrics")}
        {suffix()}
      </h2>
      <HostTiles host={props.host} />
      <h2>
        {t("serverHealth.needsAttention")}
        {suffix()}
      </h2>
      <Show
        when={attention().length > 0}
        fallback={
          <p class="sh-card sh-muted">{t("serverHealth.nothingNeeds")}</p>
        }
      >
        <ul
          class="sh-card sh-checks"
          aria-label={t("serverHealth.needsAttention")}
        >
          <For each={attention()}>
            {(c) => (
              <li class="sh-check" data-state={c.state}>
                <span class="sh-dot" data-state={c.state} aria-hidden="true" />
                <div class="sh-check-main">
                  <div class="sh-check-head">
                    <strong>{c.name}</strong>
                    <span class="sh-badge" data-state={c.state}>
                      {stateLabel(c.state)}
                    </span>
                    <span class="sh-muted sh-group">{groupLabel(c.group)}</span>
                  </div>
                  <Show when={c.detail}>
                    <p class="sh-detail">{c.detail}</p>
                  </Show>
                  <p class="sh-meta sh-muted">
                    <span title={c.since}>
                      {t("serverHealth.since", { time: duration(c.since) })}
                    </span>
                    <Show when={c.restarts_1h > 0}>
                      <span>{`· ${t("serverHealth.restarts", { count: c.restarts_1h })}`}</span>
                    </Show>
                  </p>
                </div>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={healthyCount() > 0}>
        <details class="sh-card sh-healthy">
          <summary>
            {t("serverHealth.allGood", { count: healthyCount() })}
            {suffix()}
          </summary>
          <For each={healthy()}>
            {(g) => (
              <div class="sh-group-block">
                <h3>{groupLabel(g.group)}</h3>
                <ul>
                  <For each={g.checks}>
                    {(c) => (
                      <li>
                        <span
                          class="sh-dot"
                          data-state="ok"
                          aria-hidden="true"
                        />
                        <span class="sh-name">{c.name}</span>
                        <span class="sh-muted sh-detail-inline">
                          {c.detail}
                        </span>
                      </li>
                    )}
                  </For>
                </ul>
              </div>
            )}
          </For>
        </details>
      </Show>
    </section>
  );
}

function History(props: {
  events: HealthEvent[];
  named: boolean;
  wide?: boolean;
}) {
  const [shown, setShown] = createSignal(HISTORY_PAGE);
  return (
    <Show when={props.wide} fallback={phone()}>
      <Card list title={t("serverHealth.history")}>
        <Show
          when={props.events.length}
          fallback={<EmptyState text={t("serverHealth.noHistory")} />}
        >
          <RowList label={t("serverHealth.history")}>
            <For each={props.events.slice(0, shown())}>
              {(e) => (
                <ListRow
                  title={e.name}
                  sub={e.detail}
                  meta={
                    <>
                      <time dateTime={e.at}>{clock(e.at)}</time> · {e.host} ·{" "}
                      {stateLabel(e.from)} → {stateLabel(e.to)}
                    </>
                  }
                />
              )}
            </For>
          </RowList>
          <Show when={props.events.length > shown()}>
            <Button
              variant="ghost"
              onClick={() => setShown((n) => n + HISTORY_PAGE)}
            >
              {t("serverHealth.showMore")}
            </Button>
          </Show>
        </Show>
      </Card>
    </Show>
  );
  function phone() {
    return (
      <section class="sh-history">
        <h2>{t("serverHealth.history")}</h2>
        <Show
          when={props.events.length > 0}
          fallback={
            <p class="sh-card sh-muted">{t("serverHealth.noHistory")}</p>
          }
        >
          <ol class="sh-card sh-events" aria-label={t("serverHealth.history")}>
            <For each={props.events.slice(0, shown())}>
              {(e) => (
                <li class="sh-event" data-to={e.to}>
                  <time class="sh-muted sh-event-time" dateTime={e.at}>
                    {clock(e.at)}
                  </time>
                  <div class="sh-event-main">
                    <strong>{e.name}</strong>
                    <Show when={props.named}>
                      <span class="sh-muted"> · {e.host}</span>
                    </Show>
                    <span class="sh-transition">
                      {" "}
                      {stateLabel(e.from)} →{" "}
                      <span data-state={e.to}>{stateLabel(e.to)}</span>
                    </span>
                    <Show when={e.detail}>
                      <p class="sh-detail">{e.detail}</p>
                    </Show>
                  </div>
                </li>
              )}
            </For>
          </ol>
          <Show when={props.events.length > shown()}>
            <button
              type="button"
              class="btn btn--small sh-more"
              onClick={() => setShown((n) => n + HISTORY_PAGE)}
            >
              {t("serverHealth.showMore")}
            </button>
          </Show>
        </Show>
      </section>
    );
  }
}

export default function ServerHealthView() {
  const { wide, track } = useWide();
  const [data, setData] = createSignal<SystemHealth | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);

  const load = async () => {
    if (loading()) return;
    setLoading(true);
    try {
      setData(await systemHealthApi.get());
      setError(null);
    } catch (e) {
      setError(toApiError(e).message || t("serverHealth.loadError"));
    } finally {
      setLoading(false);
    }
  };

  onMount(() => {
    void load();
    const timer = window.setInterval(() => {
      if (!document.hidden) void load();
    }, REFRESH_MS);
    const onVisible = () => {
      if (!document.hidden) void load();
    };
    document.addEventListener("visibilitychange", onVisible);
    onCleanup(() => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    });
  });

  const hosts = () => data()?.hosts ?? [];
  const named = () => hosts().length > 1;

  const attention = createMemo(() =>
    hosts().flatMap((host) =>
      needsAttention(host.snapshot.checks).map((check) => ({ host, check })),
    ),
  );
  const latestReport = () =>
    hosts()
      .map((h) => h.received_at)
      .sort()
      .at(-1);
  return (
    <div ref={track} class="sh-page-root">
      <Show when={wide()} fallback={<section class="sh-page" aria-labelledby="sh-title" aria-busy={loading()}>{phone()}</section>}>
        <Page>
          <PageHeader
            title={t("serverHealth.title")}
            context={
              latestReport()
                ? t("serverHealth.lastReport", {
                    time: relativeTime(Date.parse(latestReport()!)),
                  })
                : undefined
            }
            action={
              <button
                type="button"
                class="pg-icon-btn"
                onClick={() => void load()}
                disabled={loading()}
                aria-label={t("serverHealth.refresh")}
              >
                <RefreshIcon />
              </button>
            }
          />
          <a href="/plugins" class="pg-link pg-back">
            <BackIcon /> {t("plugins.title")}
          </a>
          <Show when={error()}>
            {(e) => (
              <Show
                when={data()}
                fallback={
                  <ErrorState
                    message={e()}
                    retryLabel={t("serverHealth.retry")}
                    onRetry={() => void load()}
                  />
                }
              >
                <p class="pg-notice" role="alert">
                  {e()}
                </p>
              </Show>
            )}
          </Show>
          <Show when={!data() && !error()}>
            <LoadingState label={t("serverHealth.loading")} />
          </Show>
          <Show when={data()}>
            <PageGrid
              layout="rail"
              rail={
                <Card list title={t("serverHealth.hosts")}>
                  <Show
                    when={hosts().length}
                    fallback={<EmptyState text={t("serverHealth.empty")} />}
                  >
                    <RowList>
                      <For each={hosts()}>
                        {(h) => (
                          <ListRow
                            title={h.host}
                            badge={
                              <span
                                class="health-status"
                                data-state={hostState(h)}
                              >
                                {h.stale
                                  ? t("serverHealth.unreachableSince", {
                                      time: clock(h.received_at),
                                    })
                                  : stateLabel(h.snapshot.overall)}
                              </span>
                            }
                            sub={t("serverHealth.lastReport", {
                              time: relativeTime(Date.parse(h.received_at)),
                            })}
                          />
                        )}
                      </For>
                    </RowList>
                  </Show>
                </Card>
              }
            >
              <Card list title={t("serverHealth.needsAttention")}>
                <Show
                  when={attention().length}
                  fallback={
                    <EmptyState text={t("serverHealth.nothingNeeds")} />
                  }
                >
                  <RowList label={t("serverHealth.needsAttention")}>
                    <For each={attention()}>
                      {({ host, check: c }) => (
                        <ListRow
                          title={c.name}
                          badge={
                            <span class="health-status" data-state={c.state}>
                              {stateLabel(c.state)}
                            </span>
                          }
                          sub={
                            <>
                              {c.detail} · {groupLabel(c.group)} ·{" "}
                              <span title={c.since}>
                                {t("serverHealth.since", {
                                  time: duration(c.since),
                                })}
                              </span>
                              <Show when={c.restarts_1h > 0}>
                                {" "}
                                ·{" "}
                                {t("serverHealth.restarts", {
                                  count: c.restarts_1h,
                                })}
                              </Show>
                            </>
                          }
                          meta={host.host}
                        />
                      )}
                    </For>
                  </RowList>
                </Show>
              </Card>
              <For each={hosts()}>
                {(h) => (
                  <Card title={`${t("serverHealth.metrics")} · ${h.host}`}>
                    <dl class="pg-stats">
                      <For each={h.snapshot.metrics.disks}>
                        {(d) => (
                          <div class="pg-stat">
                            <dt>
                              {t("serverHealth.diskFree", { mount: d.mount })}
                            </dt>
                            <dd
                              class="health-status"
                              data-state={
                                d.used_pct >= 95
                                  ? "crit"
                                  : d.used_pct >= 85
                                    ? "warn"
                                    : "ok"
                              }
                            >
                              {num(d.free_gb)} GB ·{" "}
                              {t("serverHealth.diskValue", {
                                free: num(d.free_gb),
                                used: num(d.used_pct, 1),
                              })}
                            </dd>
                          </div>
                        )}
                      </For>
                      <div class="pg-stat">
                        <dt>{t("serverHealth.cpuTemp")}</dt>
                        <dd>
                          {h.snapshot.metrics.cpu_temp_c == null
                            ? t("serverHealth.unknown")
                            : `${num(h.snapshot.metrics.cpu_temp_c!, 1)} °C`}
                        </dd>
                      </div>
                      <div class="pg-stat">
                        <dt>{t("serverHealth.memory")}</dt>
                        <dd>
                          {t("serverHealth.memoryValue", {
                            used: num(h.snapshot.metrics.mem_used_pct, 1),
                          })}
                        </dd>
                      </div>
                      <div class="pg-stat">
                        <dt>{t("serverHealth.load")}</dt>
                        <dd>
                          {h.snapshot.metrics.load
                            .map((l) => num(l, 2))
                            .join(" · ")}
                        </dd>
                      </div>
                    </dl>
                    <Show when={healthyByGroup(h.snapshot.checks).length}>
                      <details>
                        <summary class="pg-link">
                          {t("serverHealth.allGood", {
                            count: healthyByGroup(h.snapshot.checks).reduce(
                              (n, g) => n + g.checks.length,
                              0,
                            ),
                          })}
                        </summary>
                        <For each={healthyByGroup(h.snapshot.checks)}>
                          {(g) => (
                            <>
                              <h3 class="pg-card-title">
                                {groupLabel(g.group)}
                              </h3>
                              <RowList>
                                <For each={g.checks}>
                                  {(c) => (
                                    <ListRow title={c.name} sub={c.detail} />
                                  )}
                                </For>
                              </RowList>
                            </>
                          )}
                        </For>
                      </details>
                    </Show>
                  </Card>
                )}
              </For>
              <History wide events={data()!.events} named={named()} />
            </PageGrid>
          </Show>
        </Page>
      </Show>
    </div>
  );
  function phone() {
    return (
      <>
        <A href="/plugins" class="sh-back">
          <BackIcon /> {t("plugins.title")}
        </A>
        <div class="sh-titlebar">
          <h1 id="sh-title">{t("serverHealth.title")}</h1>
          <button
            type="button"
            class="btn btn--small sh-refresh"
            onClick={() => void load()}
            disabled={loading()}
            aria-label={t("serverHealth.refresh")}
          >
            <RefreshIcon />
          </button>
        </div>

        <Show when={error()}>
          <div role="alert" class="sh-card sh-error">
            <p>{data() ? error() : t("serverHealth.loadError")}</p>
            <Show when={!data()}>
              <p class="sh-muted">{error()}</p>
            </Show>
            <button
              type="button"
              class="btn btn--small"
              onClick={() => void load()}
            >
              {t("serverHealth.retry")}
            </button>
          </div>
        </Show>

        <Show when={!data() && !error()}>
          <p class="sh-muted" role="status">
            {t("serverHealth.loading")}
          </p>
        </Show>

        <Show when={data()}>
          <Show
            when={hosts().length > 0}
            fallback={<p class="sh-card sh-muted">{t("serverHealth.empty")}</p>}
          >
            <ul class="sh-hosts" aria-label={t("serverHealth.title")}>
              <For each={hosts()}>
                {(h) => (
                  <li
                    class="sh-card sh-host-head"
                    data-host={h.host}
                    data-state={hostState(h)}
                  >
                    <span
                      class="sh-dot sh-dot--big"
                      data-state={hostState(h)}
                      aria-hidden="true"
                    />
                    <div>
                      <strong class="sh-host-name">{h.host}</strong>
                      <p class="sh-host-state">
                        {h.stale
                          ? t("serverHealth.unreachableSince", {
                              time: clock(h.received_at),
                            })
                          : stateLabel(h.snapshot.overall)}
                      </p>
                      <Show when={!h.stale}>
                        <p class="sh-muted sh-meta">
                          {t("serverHealth.lastReport", {
                            time: relativeTime(Date.parse(h.received_at)),
                          })}
                        </p>
                      </Show>
                    </div>
                  </li>
                )}
              </For>
            </ul>
            <For each={hosts()}>
              {(h) => <HostDetail host={h} named={named()} />}
            </For>
          </Show>
          <History events={data()!.events} named={named()} />
        </Show>
      </>
    );
  }
}
