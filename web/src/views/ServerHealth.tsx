import { A, useLocation, useNavigate, useParams, useSearchParams } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { toApiError } from "../api/client";
import { mapHref } from "../lib/agentsRoute";
import { allComponents, attentionTotal, flappingTotal, systemOverviewApi, type SourceName, type SystemOverview } from "../api/systemOverview";
import { BackIcon, RefreshIcon } from "../components/Icons";
import { relativeTime } from "../components/NotificationBell";
import { Card, EmptyState, ErrorState, LoadingState, Page, PageHeader, SectionNav, Sheet } from "../components/page";
import { AttentionDetail, AttentionRows, RestartedRecently, ComponentDetail, HealthyChecks } from "../components/server/Attention";
import { HostCard, HostDetail } from "../components/server/Hosts";
import { hostHref, type Links } from "../components/server/links";
import { Dot } from "../components/server/parts";
import { ServicesTab } from "../components/server/Services";
import { RecentCard, TimelineTab } from "../components/server/Timeline";
import { t, type MessageKey } from "../i18n";
import "../styles/server-health.css";

// The Server page (Plugins → Server): the owner's ops dashboard on GET /v1/system/overview.
// Overview (hosts, what needs attention, recent events), Services (the catalog tree), Incidents and
// Deploys (the timeline). An attention item, a host or a component opens in a sheet whose state is
// in the URL (?item=, ?host=, ?component=). Refreshes every minute while the page is visible.

export const REFRESH_MS = 60_000;
export const BASE = "/server-health";

type Section = "overview" | "services" | "incidents" | "deploys";
const SECTIONS: Section[] = ["overview", "services", "incidents", "deploys"];
const sectionPath = (s: Section) => (s === "overview" ? BASE : `${BASE}/${s}`);

const SOURCE_TEXT: Record<SourceName, MessageKey> = {
  catalog: "serverHealth.sourceCatalog",
  sentinel: "serverHealth.sourceSentinel",
  ops: "serverHealth.sourceOps",
};

type SheetParams = { item?: string; host?: string; component?: string };

export default function ServerHealthView() {
  const params = useParams<{ section?: string }>();
  const [search, setSearch] = useSearchParams<SheetParams>();
  const location = useLocation();
  const navigate = useNavigate();

  const section = (): Section => (SECTIONS.includes(params.section as Section) ? (params.section as Section) : "overview");

  const [data, setData] = createSignal<SystemOverview | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [now, setNow] = createSignal(Date.now());
  let seq = 0;

  const load = async (fresh = false) => {
    if (loading() || (!fresh && document.hidden)) return;
    const mine = ++seq;
    setLoading(true);
    try {
      const next = await systemOverviewApi.get(fresh);
      if (mine !== seq) return;
      setData(next);
      setError(null);
    } catch (e) {
      if (mine !== seq) return;
      setError(toApiError(e).message || t("serverHealth.loadError"));
    } finally {
      if (mine === seq) setLoading(false);
      setNow(Date.now());
    }
  };

  onMount(() => {
    void load();
    const timer = window.setInterval(() => {
      if (!document.hidden) void load();
    }, REFRESH_MS);
    const tick = window.setInterval(() => setNow(Date.now()), 30_000);
    const onVisible = () => {
      if (!document.hidden) void load();
    };
    document.addEventListener("visibilitychange", onVisible);
    window.addEventListener("focus", onVisible);
    onCleanup(() => {
      ++seq;
      window.clearInterval(timer);
      window.clearInterval(tick);
      document.removeEventListener("visibilitychange", onVisible);
      window.removeEventListener("focus", onVisible);
    });
  });

  const links = createMemo<Links>(() => {
    const d = data();
    const names = new Map<string, string>();
    for (const c of allComponents(d?.services ?? [])) names.set(c.id, c.name);
    return { base: sectionPath(section()), attention: d?.attention ?? [], names };
  });

  // ---- The sheet ----------------------------------------------------------------------------
  // Opened from a link inside the page → close goes back (the URL it came from); opened from a
  // reload or a push route → close drops the parameter in place.
  const sheetOpen = () => !!(search.item || search.host || search.component);
  let openedHere = false;
  createEffect(
    on(
      () => location.search,
      () => {
        openedHere = sheetOpen();
      },
      { defer: true },
    ),
  );
  const closeSheet = () => {
    if (openedHere) {
      openedHere = false;
      navigate(-1);
    } else setSearch({ item: undefined, host: undefined, component: undefined }, { replace: true });
  };

  const item = () => (search.item ? data()?.attention.find((a) => a.key === search.item) : undefined);
  const host = () => (search.host ? data()?.hosts.find((h) => h.id === search.host) : undefined);
  const component = () => (search.component ? allComponents(data()?.services ?? []).find((c) => c.id === search.component) : undefined);
  const sheetTitle = () => item()?.title ?? host()?.name ?? component()?.name ?? search.item ?? search.host ?? search.component ?? "";

  // ---- Header -------------------------------------------------------------------------------
  const status = () => {
    if (error()) return { state: "crit", text: t("serverHealth.unreachableServer") } as const;
    const d = data();
    if (!d) return null;
    const n = attentionTotal(d);
    if (n === 0) return { state: "ok", text: t("serverHealth.allNormal") } as const;
    return { state: d.overall === "crit" ? "crit" : "warn", text: t("serverHealth.needAttentionCount", { count: n }) } as const;
  };
  const updated = () => {
    const d = data();
    return d ? t("serverHealth.updated", { time: relativeTime(Date.parse(d.generated_at), now()) }) : null;
  };

  const brokenSources = () => {
    const s = data()?.sources;
    return s ? (Object.keys(SOURCE_TEXT) as SourceName[]).filter((k) => s[k] && !s[k].ok) : [];
  };

  const navItems = () =>
    SECTIONS.map((s) => ({
      label: t(`serverHealth.${s}`),
      href: sectionPath(s),
      current: section() === s,
    }));

  return (
    <Page class="sh-page" label={t("serverHealth.title")}>
      <A href="/plugins" class="pg-link pg-back">
        <BackIcon /> {t("serverHealth.back")}
      </A>
      <PageHeader
        title={t("serverHealth.title")}
        context={
          <Show when={status()} fallback={t("serverHealth.loading")}>
            {(s) => (
              <span class="sh-status" data-state={s().state}>
                <Dot state={s().state} />
                <span class="sh-status-text">{s().text}</span>
                <Show when={updated()}>
                  <span class="sh-updated"><span class="sh-updated-dot"> · </span>{updated()}</span>
                </Show>
              </span>
            )}
          </Show>
        }
        nav={<SectionNav label={t("serverHealth.sections")} items={navItems()} />}
        action={
          <button
            type="button"
            class="pg-icon-btn sh-refresh"
            classList={{ "sh-refresh--busy": loading() }}
            aria-label={t("serverHealth.refresh")}
            aria-busy={loading()}
            disabled={loading()}
            onClick={() => void load(true)}
          >
            <RefreshIcon />
          </button>
        }
      />

      <Show when={data()} fallback={<Show when={error()} fallback={<LoadingState label={t("serverHealth.loading")} />}>
        <ErrorState message={t("serverHealth.loadError")} retryLabel={t("serverHealth.retry")} onRetry={() => void load(true)} />
      </Show>}>
        {(d) => (
          <>
            <Show when={error()}>
              <p class="sh-banner sh-banner--error" role="alert">
                {t("serverHealth.refreshError", { error: error()! })}
              </p>
            </Show>
            <For each={brokenSources()}>
              {(s) => (
                <p class="sh-banner" data-source={s}>
                  {t(SOURCE_TEXT[s])}
                </p>
              )}
            </For>

            <Show when={section() === "overview"}>
              <Overview data={d()} links={links()} />
            </Show>
            <Show when={section() === "services"}>
              <ServicesTab services={d().services} links={links()} />
            </Show>
            <Show when={section() === "incidents"}>
              <TimelineTab items={d().timeline} kinds={["incident", "change"]} label={t("serverHealth.incidents")} empty={t("serverHealth.noIncidents")} />
            </Show>
            <Show when={section() === "deploys"}>
              <TimelineTab items={d().timeline} kinds={["deploy", "decision"]} label={t("serverHealth.deploys")} empty={t("serverHealth.noDeploys")} />
            </Show>
          </>
        )}
      </Show>

      <Sheet
        open={sheetOpen() && !!data()}
        onClose={closeSheet}
        title={sheetTitle()}
        size={search.host ? "wide" : "default"}
        class="sh-sheet"
        // A host's sheet opens on its charts: focus lands on this link, not on a row further down.
        tools={
          !item() && host()?.component_id ? (
            <A href={mapHref(host()!.component_id!)} class="pg-btn">
              {t("serverHealth.openInMap")}
            </A>
          ) : undefined
        }
      >
        <Show when={item()} fallback={
          <Show when={host()} fallback={
            <Show when={component()} fallback={<p class="pg-muted">{t("serverHealth.notFound")}</p>}>
              {(c) => <ComponentDetail component={c()} />}
            </Show>
          }>
            {(h) => <HostDetail host={h()} services={data()!.services} attention={data()!.attention} links={links()} />}
          </Show>
        }>
          {(a) => <AttentionDetail item={a()} links={links()} />}
        </Show>
      </Sheet>
    </Page>
  );
}

function Overview(props: { data: SystemOverview; links: Links }) {
  const d = () => props.data;
  return (
    <div class="sh-overview">
      <Show when={d().attention.some((a) => !a.flapping)} fallback={
        <Card class="sh-calm" label={t("serverHealth.needsAttention")}>
          <EmptyState title={t("serverHealth.nothingNeeds")} text={t("serverHealth.nothingNeedsText")} />
        </Card>
      }>
        <Card title={t("serverHealth.needsAttention")} list class="sh-attention">
          <AttentionRows items={d().attention.filter((a) => !a.flapping)} links={props.links} />
        </Card>
      </Show>
      <section class="sh-hosts-section" aria-labelledby="sh-hosts-title">
        <h2 id="sh-hosts-title" class="sr-only">
          {t("serverHealth.hosts")}
        </h2>
        <Show when={d().hosts.length} fallback={<Card><EmptyState text={t("serverHealth.noHosts")} /></Card>}>
          <ul class="sh-hosts">
            <For each={d().hosts}>
              {(h) => (
                <li>
                  <HostCard host={h} href={hostHref(props.links, h.id)} />
                </li>
              )}
            </For>
          </ul>
        </Show>
      </section>

      <Show when={d().attention.some((a) => a.flapping)}>
        <RestartedRecently items={d().attention.filter((a) => a.flapping)} count={flappingTotal(d())} links={props.links} />
      </Show>
      <Show when={d().checks_ok > 0}>
        <HealthyChecks count={d().checks_ok} services={d().services} links={props.links} />
      </Show>
      <RecentCard items={d().timeline} />
    </div>
  );
}
