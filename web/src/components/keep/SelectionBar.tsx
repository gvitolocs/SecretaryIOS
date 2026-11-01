import { createSignal, Match, Switch } from "solid-js";
import type { KeepNote } from "../../api/keep";
import { t } from "../../i18n";
import { keep } from "../../state/keep";
import { archive, copyNote, deleteForever, restore, setColor, setPinned, trash, unarchive } from "../../state/keepActions";
import { clearSelection } from "../../state/keepUi";
import { useKeep } from "./ctx";
import { AddAlertIcon, ArchiveIcon, CloseIcon, DeleteForeverIcon, PaletteIcon, PinFilledIcon, PinOutlineIcon, RestoreIcon, UnarchiveIcon, MoreIcon } from "./icons";
import { ColorPopover, LabelPopover, MenuPopover, ReminderPopover } from "./Pickers";

/** Replaces the top bar while notes are selected: count, pin, reminder, colour, archive, more. */
export function SelectionBar(props: { notes: KeepNote[] }) {
  const ctx = useKeep();
  const ids = () => props.notes.map((n) => n.id);
  const allPinned = () => props.notes.length > 0 && props.notes.every((n) => n.pinned);
  const allArchived = () => props.notes.length > 0 && props.notes.every((n) => n.archived);
  const inTrash = () => props.notes.length > 0 && props.notes.every((n) => n.deletedAt);
  const [open, setOpen] = createSignal<"color" | "reminder" | "more" | "labels" | null>(null);
  let colorBtn: HTMLButtonElement | undefined;
  let remindBtn: HTMLButtonElement | undefined;
  let moreBtn: HTMLButtonElement | undefined;
  const close = () => setOpen(null);
  const tagState = (tag: string) => props.notes.length > 0 && props.notes.every((n) => n.tags.includes(tag));
  const done = (fn: () => void) => () => {
    fn();
    clearSelection();
  };

  return (
    <div class="kp-selbar" role="toolbar" aria-label={t("keep.selected", { count: props.notes.length })}>
      <button type="button" class="kp-icon-btn kp-icon-btn--lg" aria-label={t("keep.clearSelection")} title={t("keep.clearSelection")} onClick={clearSelection}>
        <CloseIcon />
      </button>
      <span class="kp-selbar-count">{t("keep.selected", { count: props.notes.length })}</span>
      <span class="kp-spacer" />
      <Switch>
        <Match when={inTrash()}>
          <button type="button" class="kp-icon-btn kp-icon-btn--lg" data-tool="delete-forever" aria-label={t("keep.deleteForever")} title={t("keep.deleteForever")} onClick={done(() => deleteForever(ids()))}>
            <DeleteForeverIcon />
          </button>
          <button type="button" class="kp-icon-btn kp-icon-btn--lg" data-tool="restore" aria-label={t("keep.restore")} title={t("keep.restore")} onClick={done(() => restore(ids()))}>
            <RestoreIcon />
          </button>
        </Match>
        <Match when={true}>
          <button type="button" class="kp-icon-btn kp-icon-btn--lg" data-tool="pin" aria-pressed={allPinned()} aria-label={allPinned() ? t("keep.unpin") : t("keep.pin")} title={allPinned() ? t("keep.unpin") : t("keep.pin")} onClick={done(() => setPinned(ids(), !allPinned()))}>
            {allPinned() ? <PinFilledIcon /> : <PinOutlineIcon />}
          </button>
          <button ref={remindBtn} type="button" class="kp-icon-btn kp-icon-btn--lg" data-tool="reminder" aria-label={t("keep.remindMe")} title={t("keep.remindMe")} onClick={() => setOpen(open() === "reminder" ? null : "reminder")}>
            <AddAlertIcon />
          </button>
          <button ref={colorBtn} type="button" class="kp-icon-btn kp-icon-btn--lg" data-tool="color" aria-label={t("keep.background")} title={t("keep.background")} onClick={() => setOpen(open() === "color" ? null : "color")}>
            <PaletteIcon />
          </button>
          <button type="button" class="kp-icon-btn kp-icon-btn--lg" data-tool="archive" aria-label={allArchived() ? t("keep.unarchiveAct") : t("keep.archiveAct")} title={allArchived() ? t("keep.unarchiveAct") : t("keep.archiveAct")} onClick={done(() => (allArchived() ? unarchive(ids()) : archive(ids())))}>
            {allArchived() ? <UnarchiveIcon /> : <ArchiveIcon />}
          </button>
          <button ref={moreBtn} type="button" class="kp-icon-btn kp-icon-btn--lg" data-tool="more" aria-label={t("keep.more")} title={t("keep.more")} onClick={() => setOpen(open() === "more" ? null : "more")}>
            <MoreIcon />
          </button>
        </Match>
      </Switch>
      <Switch>
        <Match when={open() === "color"}>
          <ColorPopover anchor={colorBtn} value={props.notes.length && props.notes.every((n) => n.color === props.notes[0]?.color) ? (props.notes[0]?.color ?? "default") : "default"} theme={ctx.theme()} onPick={(c) => setColor(ids(), c)} onClose={close} />
        </Match>
        <Match when={open() === "reminder"}>
          <ReminderPopover anchor={remindBtn} onSave={(r) => void keep.updateMany(ids(), { reminder: r })} onClose={close} />
        </Match>
        <Match when={open() === "more"}>
          <MenuPopover
            anchor={moreBtn}
            label={t("keep.more")}
            entries={[
              { id: "delete", label: t("keep.deleteNote"), run: done(() => trash(ids())) },
              { id: "labels", label: t("keep.addLabel"), run: () => setOpen("labels") },
              ...(props.notes.length === 1 ? [{ id: "copy", label: t("keep.makeCopy"), run: done(() => void copyNote(props.notes[0]!.id)) }] : []),
            ]}
            onClose={close}
          />
        </Match>
        <Match when={open() === "labels"}>
          <LabelPopover
            anchor={moreBtn}
            all={keep.labels()}
            selected={keep.labels().filter((l) => tagState(l.tag)).map((l) => l.tag)}
            onToggle={(l, on) => void (on ? keep.addLabel(ids(), l.name) : keep.removeLabel(ids(), l.tag))}
            onCreate={(name) => void keep.addLabel(ids(), name)}
            onClose={close}
          />
        </Match>
      </Switch>
    </div>
  );
}
