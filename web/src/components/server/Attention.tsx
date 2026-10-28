import { A, useNavigate } from "@solidjs/router";
import { For, Show } from "solid-js";
import { duration, healthyByGroup, type AttentionItem, type Ref, type ServiceComponent, type ServiceGroup } from "../../api/systemOverview";
import { locale, t } from "../../i18n";
import { mapHref } from "../../lib/agentsRoute";
import { requestPrefill } from "../../state/chat";
import { ListRow, RowList } from "../page";
import { Markdown } from "../Markdown";
import { componentHref, itemHref, SERVICES_PATH, type Links } from "./links";
import { Chip, ChevronIcon, clock, CopyCommand, Dot, forTime, levelLabel, stateLabel, StateWord } from "./parts";

// What needs attention: the rows on the Overview (and in a host's sheet), an item's sheet with its
// impact and quick actions, a plain component's sheet, and the collapsed list of healthy checks.

export function AttentionRows(props: { items: AttentionItem[]; links: Links; hideHost?: boolean }) {
  return (
    <RowList label={t("serverHealth.needsAttention")}>
      <For each={props.items}>
        {(a) => (
          <ListRow
            class={`sh-arow sh-arow--${a.severity}`}
            href={itemHref(props.links, a.key)}
            title={a.title}
            badge={stateLabel(a.severity)}
            leading={<Dot state={a.severity} />}
            sub={a.summary}
            meta={
              <>
                <span class="sh-meta-line">{forTime(a.since)}</span>
                <Show when={a.impact && (a.impact.broken.length + (a.severity === "warn" ? a.impact.degraded.length : 0))}>
                  <span class="sh-meta-line sh-breaks">{t(a.severity === "warn" ? "serverHealth.affects" : "serverHealth.breaks", { count: a.impact!.broken.length + (a.severity === "warn" ? a.impact!.degraded.length : 0) })}</span>
                </Show>
              </>
            }
            trailing={
              <>
                <span class="sh-arow-chips">
                  <Show when={!props.hideHost && a.host}>
                    <Chip>{a.host}</Chip>
                  </Show>
                  <Show when={a.project}>
                    <Chip>{a.project}</Chip>
                  </Show>
                </span>
                <ChevronIcon />
              </>
            }
          />
        )}
      </For>
    </RowList>
  );
}

/** The short context Ask Lucina puts in the composer. */
export function askText(a: AttentionItem): string {
  const lines = [t("serverHealth.askIntro"), t("serverHealth.askTitle", { title: a.title })];
  if (a.host) lines.push(t("serverHealth.askHost", { host: a.host }));
  lines.push(t("serverHealth.askSummary", { summary: a.summary }));
  lines.push(t("serverHealth.askSince", { time: `${clock(a.since)} (${duration(a.since, Date.now(), locale())})` }));
  if (a.impact?.broken.length) lines.push(t("serverHealth.askBreaks", { list: a.impact.broken.map((b) => b.name).join(", ") }));
  if (a.logs) lines.push(t("serverHealth.askLogs", { command: a.logs }));
  lines.push(t("serverHealth.askOutro"));
  return lines.join("\n");
}

function RefLinks(props: { refs: Ref[]; links: Links }) {
  return (
    <ul class="sh-reflist">
      <For each={props.refs}>
        {(r) => (
          <li>
            <A href={componentHref(props.links, r.id, SERVICES_PATH)}>{r.name}</A>
          </li>
        )}
      </For>
    </ul>
  );
}

export function AttentionDetail(props: { item: AttentionItem; links: Links }) {
  const navigate = useNavigate();
  const a = () => props.item;
  const ask = () => {
    requestPrefill(askText(a()));
    navigate("/");
  };
  const impact = () => a().impact;
  const hasImpact = () => !!impact() && (impact()!.broken.length > 0 || impact()!.degraded.length > 0 || impact()!.public_hostnames_broken.length > 0);
  return (
    <div class="sh-detail">
      <div class="sh-detail-status">
        <Dot state={a().severity} />
        <span>{stateLabel(a().severity)}</span>
        <Show when={a().host}>
          <Chip>{a().host}</Chip>
        </Show>
        <Show when={a().project}>
          <Chip>{a().project}</Chip>
        </Show>
        <Show when={a().criticality}>
          <Chip>{levelLabel(a().criticality!)}</Chip>
        </Show>
      </div>
      <p class="sh-summary">{a().summary}</p>
      <p class="pg-muted">
        <time dateTime={a().since}>{t("serverHealth.since", { time: clock(a().since), duration: duration(a().since, Date.now(), locale()) })}</time>
        <Show when={a().failures_24h != null}>
          <span> · {t("serverHealth.failures", { count: a().failures_24h! })}</span>
        </Show>
      </p>

      <div class="sh-actions" role="group" aria-label={t("serverHealth.actions")}>
        <button type="button" class="pg-cta" onClick={ask}>
          {t("serverHealth.askLucina")}
        </button>
        <Show when={a().component_id}>
          {(id) => (
            <A href={mapHref(id())} class="pg-btn">
              {t("serverHealth.openInMap")}
            </A>
          )}
        </Show>
      </div>

      <Show when={a().logs}>{(cmd) => <CopyCommand label={t("serverHealth.logsCommand")} copyLabel={t("serverHealth.copyLogs")} command={cmd()} />}</Show>
      <Show when={a().restart}>{(cmd) => <CopyCommand label={t("serverHealth.restartCommand")} copyLabel={t("serverHealth.copyRestart")} command={cmd()} />}</Show>

      <Show when={a().details.length}>
        <section class="sh-section">
          <h3>{t("serverHealth.details")}</h3>
          <ul class="sh-bullets">
            <For each={a().details}>{(d) => <li>{d}</li>}</For>
          </ul>
        </section>
      </Show>

      <Show when={hasImpact()}>
        <section class="sh-section">
          <h3>{t(a().severity === "warn" ? "serverHealth.ifDown" : "serverHealth.impact")}</h3>
          <dl class="sh-impact">
            <Show when={impact()!.broken.length}>
              <dt>{t(a().severity === "warn" ? "serverHealth.wouldBreak" : "serverHealth.impactBreaks")}</dt>
              <dd>
                <RefLinks refs={impact()!.broken} links={props.links} />
              </dd>
            </Show>
            <Show when={impact()!.degraded.length}>
              <dt>{t(a().severity === "warn" ? "serverHealth.wouldDegrade" : "serverHealth.impactDegrades")}</dt>
              <dd>
                <RefLinks refs={impact()!.degraded} links={props.links} />
              </dd>
            </Show>
            <Show when={impact()!.public_hostnames_broken.length}>
              <dt>{t("serverHealth.impactPublic")}</dt>
              <dd>
                <ul class="sh-reflist">
                  <For each={impact()!.public_hostnames_broken}>{(h) => <li>{h}</li>}</For>
                </ul>
              </dd>
            </Show>
          </dl>
        </section>
      </Show>

      <Show when={a().caused.length}>
        <section class="sh-section">
          <h3>{t("serverHealth.caused")}</h3>
          <RefLinks refs={a().caused.map((id) => ({ id, name: props.links.names.get(id) ?? id }))} links={props.links} />
        </section>
      </Show>

      <Show when={a().runbook}>
        {(md) => (
          <section class="sh-section">
            <h3>{t("serverHealth.runbook")}</h3>
            <Markdown text={md()} class="sh-runbook" />
          </section>
        )}
      </Show>
    </div>
  );
}

export function ComponentDetail(props: { component: ServiceComponent & { group: string; project: string } }) {
  const c = () => props.component;
  return (
    <div class="sh-detail">
      <div class="sh-detail-status">
        <Dot state={c().state} />
        <span>{stateLabel(c().state)}</span>
        <Show when={c().host}>
          <Chip>{c().host}</Chip>
        </Show>
        <Chip>{c().project}</Chip>
      </div>
      <Show when={c().description}>
        <p class="sh-summary">{c().description}</p>
      </Show>
      <dl class="pg-stats">
        <div class="pg-stat">
          <dt>{t("serverHealth.kind")}</dt>
          <dd>{c().kind}</dd>
        </div>
        <Show when={c().host}>
          <div class="pg-stat">
            <dt>{t("serverHealth.host")}</dt>
            <dd>{c().host}</dd>
          </div>
        </Show>
        <div class="pg-stat">
          <dt>{t("serverHealth.group")}</dt>
          <dd>{c().group}</dd>
        </div>
        <Show when={c().criticality}>
          <div class="pg-stat">
            <dt>{t("serverHealth.criticalityLabel")}</dt>
            <dd>{levelLabel(c().criticality!)}</dd>
          </div>
        </Show>
      </dl>
      <div class="sh-actions">
        <A href={mapHref(c().id)} class="pg-btn">
          {t("serverHealth.openInMap")}
        </A>
      </div>
    </div>
  );
}

/** "All good · 122 checks": the healthy components by group, collapsed. */
export function HealthyChecks(props: { count: number; services: ServiceGroup[]; links: Links }) {
  const groups = () => healthyByGroup(props.services);
  return (
    <details class="pg-card sh-healthy">
      <summary>
        <Dot state="ok" />
        <span>{t("serverHealth.allGood", { count: props.count })}</span>
      </summary>
      <div class="sh-healthy-groups">
        <For each={groups()}>
          {(g) => (
            <section class="sh-healthy-group">
              <h3>{g.group}</h3>
              <ul>
                <For each={g.components}>
                  {(c) => (
                    <li>
                      <A href={componentHref(props.links, c.id)}>
                        <Dot state={c.state} />
                        <span>{c.name}</span>
                        <StateWord state={c.state} />
                      </A>
                    </li>
                  )}
                </For>
              </ul>
            </section>
          )}
        </For>
      </div>
    </details>
  );
}

export function RestartedRecently(props: { items: AttentionItem[]; count: number; links: Links }) {
  return (
    <details class="pg-card sh-healthy sh-restarted">
      <summary>{t("serverHealth.restartedRecently", { count: props.count })}</summary>
      <ul class="sh-restarted-list">
        <For each={props.items}>{(a) => (
          <li><A href={itemHref(props.links, a.key)}>
            <span class="sh-restarted-title">{a.title}</span>
            <span>{t("serverHealth.failedTimes", { count: a.failures_24h ?? 0 })}</span>
            <Show when={a.host}><Chip>{a.host}</Chip></Show>
          </A></li>
        )}</For>
      </ul>
    </details>
  );
}
