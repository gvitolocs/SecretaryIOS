import { A } from "@solidjs/router";
import { createMemo, For, Show } from "solid-js";
import { allComponents, diskLevel, type AttentionItem, type Disk, type Host, type ServiceGroup } from "../../api/systemOverview";
import { t } from "../../i18n";
import { BigChart, Spark } from "./Chart";
import { ago, Chip, clock, Dot, gb, num, stateLabel, StateWord } from "./parts";
import { AttentionRows } from "./Attention";
import { componentHref, type Links } from "./links";

// The hosts row on the Overview (compact cards) and a host's detail in the sheet.

function osLine(h: Host): string {
  return [h.os, h.kernel].filter(Boolean).join(" · ");
}

function HostMeta(props: { host: Host }) {
  const h = () => props.host;
  return (
    <p class="sh-host-meta">
      <Show when={h().uptime}>
        <span>{t("serverHealth.uptime", { time: h().uptime! })}</span>
      </Show>
      <span>{h().last_report ? t("serverHealth.reported", { time: ago(h().last_report!) }) : t("serverHealth.noReport")}</span>
    </p>
  );
}

/** Why a host shows no charts, or null when it has data. */
function hostNote(h: Host): string | null {
  if (!h.reachable) return h.last_report ? t("serverHealth.unreachableSince", { time: clock(h.last_report) }) : t("serverHealth.unreachable");
  if (h.limited) return t("serverHealth.limited");
  return null;
}

export function DiskBars(props: { disks: Disk[]; more?: number }) {
  return (
    <ul class="sh-disks">
      <For each={props.disks}>
        {(d) => (
          <li class="sh-disk" data-level={diskLevel(d.used_pct)} aria-label={t("serverHealth.diskLabel", { mount: d.mount, used: num(d.used_pct), free: gb(d.free_gb) })}>
            <div class="sh-disk-text" aria-hidden="true">
              <span class="sh-disk-mount">{d.mount}</span>
              <span class="sh-disk-free">
                <span class="sh-disk-pct">{num(d.used_pct)}% · </span>
                {t("serverHealth.diskFree", { free: gb(d.free_gb) })}
                <Show when={props.more}> · {t("serverHealth.moreDisks", { count: props.more! })}</Show>
              </span>
            </div>
            <div class="sh-bar" aria-hidden="true">
              <span style={{ width: `${Math.min(100, Math.max(0, d.used_pct))}%` }} />
            </div>
          </li>
        )}
      </For>
    </ul>
  );
}

export function HostCard(props: { host: Host; href: string }) {
  const h = () => props.host;
  const note = () => hostNote(h());
  return (
    <A href={props.href} class="sh-host" data-state={h().state} data-host={h().id}>
      <div class="sh-host-head">
        <Dot state={h().state} />
        <strong class="sh-host-name">{h().name}</strong>
        <Show when={h().role}>
          <span class="sh-host-role">{h().role}</span>
        </Show>
        <span class="sr-only">{stateLabel(h().state)}</span>
        <Show when={h().uptime}><span class="sh-host-uptime">{t("serverHealth.uptime", { time: h().uptime! })}</span></Show>
      </div>
      <Show when={osLine(h())}>
        <p class="sh-host-os" title={osLine(h())}>
          {osLine(h())}
        </p>
      </Show>
      <Show
        when={!note()}
        fallback={
          <p class="sh-host-note" data-state={h().state}>
            {note()}
          </p>
        }
      >
        <div class="sh-sparks">
          <Spark metric="tctl" values={h().history?.tctl ?? []} current={h().tctl} />
          <Spark metric="load1" values={h().history?.load1 ?? []} current={h().load?.[0] ?? null} />
          <Spark metric="mem" values={h().history?.mem ?? []} current={h().mem_used_pct} />
        </div>
        <Show when={h().disks.length}>
          <DiskBars disks={[...h().disks].sort((a, b) => b.used_pct - a.used_pct).slice(0, 1)} more={h().disks.length - 1} />
        </Show>
      </Show>
    </A>
  );
}

export function HostDetail(props: { host: Host; services: ServiceGroup[]; attention: AttentionItem[]; links: Links }) {
  const h = () => props.host;
  const components = createMemo(() => allComponents(props.services).filter((c) => c.host === h().id));
  const attention = createMemo(() => props.attention.filter((a) => a.host === h().id && !a.flapping));
  return (
    <div class="sh-detail">
      <div class="sh-detail-status">
        <Dot state={h().state} />
        <span>{hostNote(h()) ?? stateLabel(h().state)}</span>
        <Show when={h().role}>
          <span class="pg-muted">· {h().role}</span>
        </Show>
      </div>
      <HostMeta host={h()} />
      <Show when={osLine(h()) || h().cpus}>
        <p class="sh-host-os">
          {osLine(h())}
          <Show when={h().cpus}> · {t("serverHealth.cpus", { count: h().cpus! })}</Show>
        </p>
      </Show>

      <Show when={h().history && !hostNote(h())}>
        <div class="sh-bigcharts">
          <BigChart metric="tctl" history={h().history!} current={h().tctl} />
          <BigChart metric="load1" history={h().history!} current={h().load?.[0] ?? null} />
          <BigChart metric="mem" history={h().history!} current={h().mem_used_pct} />
        </div>
      </Show>

      <Show when={h().disks.length}>
        <section class="sh-section">
          <h3>{t("serverHealth.disks")}</h3>
          <DiskBars disks={h().disks} />
        </section>
      </Show>

      <Show when={h().findings.length}>
        <section class="sh-section">
          <h3>{t("serverHealth.findings")}</h3>
          <ul class="sh-bullets">
            <For each={h().findings}>{(f) => <li>{f}</li>}</For>
          </ul>
        </section>
      </Show>

      <Show when={attention().length}>
        <section class="sh-section">
          <h3>{t("serverHealth.hostAttention")}</h3>
          <div class="sh-inset">
            <AttentionRows items={attention()} links={props.links} hideHost />
          </div>
        </section>
      </Show>

      <section class="sh-section">
        <h3>{t("serverHealth.components")}</h3>
        <Show when={components().length} fallback={<p class="pg-muted">{t("serverHealth.noComponents")}</p>}>
          <ul class="sh-complist">
            <For each={components()}>
              {(c) => (
                <li>
                  <A href={componentHref(props.links, c.id)} class="sh-comp-link">
                    <Dot state={c.state} />
                    <span class="sh-comp-name">{c.name}</span>
                    <StateWord state={c.state} />
                    <Chip>{c.project}</Chip>
                  </A>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </section>
    </div>
  );
}
