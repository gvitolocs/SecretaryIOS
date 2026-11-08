import { createSignal, For, Show, type JSX } from "solid-js";
import type { LabelInfo } from "../../api/keep";
import { t } from "../../i18n";
import { sortLabels, type KeepView } from "../../lib/keepFilter";
import { keepHref, sameView } from "../../lib/keepRoute";
import { ContextMenu, menuMode, type MenuRequest } from "../ContextMenu";
import { Card, RowList } from "../page";
import { ArchiveIcon, BellIcon, BulbOutlineIcon, EditIcon, LabelIcon, TrashIcon } from "./icons";

// Keep's sections on the Lucina page layout (owner, 2026-10-31: "completely migrate the layout to be Lucina"):
// a chip row on a phone or a narrow window, a rail card on a desktop. Labels are folded away in both: one "Labels"
// chip that opens the list as a menu, or one rail row that expands, instead of every label in a long column.

const go = (onGo: (href: string) => void, href: string) => (e: MouseEvent) => {
  if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
  e.preventDefault();
  onGo(href);
};

interface NavProps {
  view: KeepView;
  labels: LabelInfo[];
  labelName: (tag: string) => string;
  onGo: (href: string) => void;
  onEditLabels: () => void;
}

/** Phone and narrow windows: Notes · Reminders · Labels ▾ · Archive · Trash as the shared chips. */
export function KeepNavChips(props: NavProps) {
  const [menu, setMenu] = createSignal<MenuRequest | null>(null);
  const is = (v: KeepView) => sameView(props.view, v);
  const onLabel = () => props.view.kind === "label";
  const chip = (href: string, label: string, v: KeepView) => (
    <a href={href} class="chip" aria-current={is(v) ? "page" : undefined} onClick={go(props.onGo, href)}>
      {label}
    </a>
  );
  return (
    <nav class="chip-group kp-navchips" aria-label={t("keep.sections")}>
      {chip("/keep", t("keep.notes"), { kind: "notes" })}
      {chip("/keep/reminders", t("keep.reminders"), { kind: "reminders" })}
      <button
        type="button"
        class="chip"
        aria-pressed={onLabel()}
        aria-haspopup="menu"
        onClick={(e) => {
          const r = e.currentTarget.getBoundingClientRect();
          setMenu({ x: r.left, y: r.bottom + 4, mode: menuMode(), title: t("keep.labels") });
        }}
      >
        {onLabel() ? props.labelName((props.view as { tag: string }).tag) : t("keep.labels")}
        <span aria-hidden="true">▾</span>
      </button>
      {chip("/keep/archive", t("keep.archive"), { kind: "archive" })}
      {chip("/keep/trash", t("keep.trash"), { kind: "trash" })}
      <ContextMenu
        request={menu()}
        label={t("keep.labels")}
        onClose={() => setMenu(null)}
        actions={[
          ...sortLabels(props.labels).map((l) => ({
            id: `label-${l.tag}`,
            label: l.name,
            icon: <LabelIcon />,
            run: () => props.onGo(keepHref({ kind: "label", tag: l.tag }, l.name)),
          })),
          { id: "edit-labels", label: t("keep.editLabels"), icon: <EditIcon />, separator: props.labels.length > 0, run: props.onEditLabels },
        ]}
      />
    </nav>
  );
}

function RailRow(props: { href?: string; label: string; icon: JSX.Element; active?: boolean; onGo?: (href: string) => void; children?: JSX.Element }) {
  return (
    <li class="pg-row kp-railrow" classList={{ "kp-railrow--active": !!props.active }}>
      <span class="kp-railrow-icon" aria-hidden="true">
        {props.icon}
      </span>
      <div class="pg-row-main">
        <p class="pg-row-title">
          <Show when={props.href} fallback={props.children}>
            {(href) => (
              <a href={href()} class="pg-row-name pg-row-link" aria-current={props.active ? "page" : undefined} onClick={go(props.onGo!, href())}>
                {props.label}
              </a>
            )}
          </Show>
        </p>
      </div>
    </li>
  );
}

/** Desktop: one rail card. Labels stay folded until opened, or while one of them is the current view. */
export function KeepRailNav(props: NavProps) {
  const is = (v: KeepView) => sameView(props.view, v);
  const [open, setOpen] = createSignal(false);
  const expanded = () => open() || props.view.kind === "label";
  return (
    <Card list label={t("keep.sections")}>
      <RowList>
        <RailRow href="/keep" label={t("keep.notes")} icon={<BulbOutlineIcon />} active={is({ kind: "notes" })} onGo={props.onGo} />
        <RailRow href="/keep/reminders" label={t("keep.reminders")} icon={<BellIcon />} active={is({ kind: "reminders" })} onGo={props.onGo} />
        <RailRow label={t("keep.labels")} icon={<LabelIcon />}>
          <button
            type="button"
            class="pg-row-name pg-row-link kp-railrow-toggle"
            aria-expanded={expanded()}
            aria-label={expanded() ? t("keep.labelsHide") : t("keep.labelsShow")}
            onClick={() => setOpen(!expanded())}
          >
            {t("keep.labels")}
            <span class="kp-railrow-count">{props.labels.length}</span>
            <span class="kp-railrow-caret" aria-hidden="true">
              {expanded() ? "▴" : "▾"}
            </span>
          </button>
        </RailRow>
        <Show when={expanded()}>
          <li class="kp-raillabels">
            <ul>
              <For each={sortLabels(props.labels)}>
                {(l) => {
                  const href = keepHref({ kind: "label", tag: l.tag }, l.name);
                  return (
                    <li>
                      <a href={href} class="kp-raillabel" aria-current={is({ kind: "label", tag: l.tag }) ? "page" : undefined} onClick={go(props.onGo, href)}>
                        {l.name}
                      </a>
                    </li>
                  );
                }}
              </For>
              <li>
                <button type="button" class="kp-raillabel kp-raillabel--edit" onClick={props.onEditLabels}>
                  {t("keep.editLabels")}
                </button>
              </li>
            </ul>
          </li>
        </Show>
        <RailRow href="/keep/archive" label={t("keep.archive")} icon={<ArchiveIcon />} active={is({ kind: "archive" })} onGo={props.onGo} />
        <RailRow href="/keep/trash" label={t("keep.trash")} icon={<TrashIcon />} active={is({ kind: "trash" })} onGo={props.onGo} />
      </RowList>
    </Card>
  );
}
