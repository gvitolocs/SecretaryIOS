import { createMemo, For, Show } from "solid-js";
import { dayKey, dayRelation, groupByDay, type TimelineItem, type TimelineKind } from "../../api/systemOverview";
import { locale, t } from "../../i18n";
import { Card, EmptyState } from "../page";
import { Chip, KindIcon, levelLabel, stateLabel } from "./parts";

// The timeline: incidents and state changes, or deploys and decisions, grouped by day; and the
// latest few items of any kind in the Overview's "Recent" column.

function dayLabel(day: string): string {
  const rel = dayRelation(day);
  if (rel === "today") return t("serverHealth.today");
  if (rel === "yesterday") return t("serverHealth.yesterday");
  const [y, m, d] = day.split("-").map(Number);
  return new Intl.DateTimeFormat(locale(), { weekday: "short", day: "numeric", month: "short", year: y === new Date().getFullYear() ? undefined : "numeric" }).format(new Date(y!, m! - 1, d!));
}

const hhmm = (iso: string) => new Intl.DateTimeFormat(locale(), { hour: "2-digit", minute: "2-digit" }).format(new Date(iso));

export function TimelineRow(props: { item: TimelineItem; withDay?: boolean }) {
  const i = () => props.item;
  const when = () => {
    const day = dayKey(i().at);
    return props.withDay && dayRelation(day) !== "today" ? `${dayLabel(day)} ${hhmm(i().at)}` : hhmm(i().at);
  };
  return (
    <li class="sh-tl-row" data-kind={i().kind}>
      <time class="sh-tl-time" dateTime={i().at}>
        {when()}
      </time>
      <KindIcon kind={i().kind} />
      <div class="sh-tl-main">
        <p class="sh-tl-title">{i().title}</p>
        <Show when={i().detail}>
          <p class="sh-tl-detail">{i().detail}</p>
        </Show>
        <p class="sh-tl-tags">
          <Show when={i().severity}>
            <span class="sh-sev" data-level={i().severity}>
              {levelLabel(i().severity!)}
            </span>
          </Show>
          <Show when={i().from && i().to}>
            <span class="sh-flip">
              <span data-state={i().from}>{stateLabel(i().from!)}</span>
              {" → "}
              <span data-state={i().to}>{stateLabel(i().to!)}</span>
            </span>
          </Show>
          <Show when={i().project}>
            <Chip>{i().project}</Chip>
          </Show>
          <Show when={i().host && i().host !== i().project}>
            <Chip>{i().host}</Chip>
          </Show>
        </p>
      </div>
    </li>
  );
}

export function TimelineTab(props: { items: TimelineItem[]; kinds: TimelineKind[]; label: string; empty: string }) {
  const days = createMemo(() => groupByDay(props.items, props.kinds));
  return (
    <Show
      when={days().length}
      fallback={
        <Card>
          <EmptyState text={props.empty} />
        </Card>
      }
    >
      <div class="sh-timeline" aria-label={props.label} role="region">
        <For each={days()}>
          {(d) => (
            <Card title={dayLabel(d.day)} list class="sh-day">
              <ol class="sh-tl">
                <For each={d.items}>{(item) => <TimelineRow item={item} />}</For>
              </ol>
            </Card>
          )}
        </For>
      </div>
    </Show>
  );
}

export function RecentCard(props: { items: TimelineItem[] }) {
  const latest = () => [...props.items].sort((a, b) => Date.parse(b.at) - Date.parse(a.at)).slice(0, 5);
  return (
    <Card title={t("serverHealth.recent")} list class="sh-recent">
      <Show when={latest().length} fallback={<EmptyState text={t("serverHealth.noRecent")} />}>
        <ol class="sh-tl" aria-label={t("serverHealth.recent")}>
          <For each={latest()}>{(item) => <TimelineRow item={item} withDay />}</For>
        </ol>
      </Show>
    </Card>
  );
}
