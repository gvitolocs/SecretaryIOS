import { createEffect, For, type JSX } from "solid-js";
import { t } from "../../i18n";
import { sameView, keepHref } from "../../lib/keepRoute";
import type { KeepView } from "../../lib/keepFilter";
import type { LabelInfo } from "../../api/keep";
import { ArchiveIcon, BellIcon, BulbOutlineIcon, EditIcon, LabelIcon, TrashIcon } from "./icons";

function Item(props: { href: string; label: string; icon: JSX.Element; active: boolean; onGo: (href: string) => void; data?: string }) {
  return (
    <a
      class="kp-nav-item"
      classList={{ "kp-nav-item--active": props.active }}
      href={props.href}
      aria-current={props.active ? "page" : undefined}
      data-nav={props.data}
      title={props.label}
      onClick={(e) => {
        if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
        e.preventDefault();
        props.onGo(props.href);
      }}
    >
      <span class="kp-nav-icon">{props.icon}</span>
      <span class="kp-nav-label">{props.label}</span>
    </a>
  );
}

/** Keep's left nav: Notes, Reminders, the labels, Edit labels, Archive, Trash. Collapsed it shows icons and opens on hover. */
export function Nav(props: { view: KeepView; labels: LabelInfo[]; collapsed: boolean; onGo: (href: string) => void; onEditLabels: () => void }) {
  const is = (v: KeepView) => sameView(props.view, v);
  let inner: HTMLDivElement | undefined;
  // With many labels the active item (Archive, Trash) can sit below the fold: bring it into view.
  createEffect(() => {
    void props.view;
    queueMicrotask(() => inner?.querySelector<HTMLElement>(".kp-nav-item--active")?.scrollIntoView?.({ block: "nearest" }));
  });
  return (
    <nav class="kp-nav" classList={{ "kp-nav--collapsed": props.collapsed }} aria-label={t("keep.menu")}>
      <div class="kp-nav-inner" ref={inner}>
        <Item href="/keep" label={t("keep.notes")} icon={<BulbOutlineIcon />} active={is({ kind: "notes" })} onGo={props.onGo} data="notes" />
        <Item href="/keep/reminders" label={t("keep.reminders")} icon={<BellIcon />} active={is({ kind: "reminders" })} onGo={props.onGo} data="reminders" />
        <For each={props.labels}>
          {(l) => <Item href={keepHref({ kind: "label", tag: l.tag }, l.name)} label={l.name} icon={<LabelIcon />} active={is({ kind: "label", tag: l.tag })} onGo={props.onGo} data={`label:${l.tag}`} />}
        </For>
        <button type="button" class="kp-nav-item" onClick={props.onEditLabels} title={t("keep.editLabels")} data-nav="edit-labels">
          <span class="kp-nav-icon">
            <EditIcon />
          </span>
          <span class="kp-nav-label">{t("keep.editLabels")}</span>
        </button>
        <Item href="/keep/archive" label={t("keep.archive")} icon={<ArchiveIcon />} active={is({ kind: "archive" })} onGo={props.onGo} data="archive" />
        <Item href="/keep/trash" label={t("keep.trash")} icon={<TrashIcon />} active={is({ kind: "trash" })} onGo={props.onGo} data="trash" />
      </div>
    </nav>
  );
}
