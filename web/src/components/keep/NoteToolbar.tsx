import { createSignal, Match, Switch } from "solid-js";
import { ALLOWED_IMAGE_TYPES, type KeepColor, type LabelInfo, type Reminder } from "../../api/keep";
import { t } from "../../i18n";
import type { KeepTheme } from "../../lib/keepColors";
import { AddAlertIcon, ArchiveIcon, ImageIcon, MoreIcon, PaletteIcon, PersonAddIcon, UnarchiveIcon } from "./icons";
import { CollaboratorPopover, ColorPopover, LabelPopover, MenuPopover, ReminderPopover, type MenuEntry } from "./Pickers";

export interface ToolbarProps {
  theme: KeepTheme;
  color: KeepColor;
  onColor: (c: KeepColor) => void;
  reminder?: Reminder | undefined;
  onReminder: (r: Reminder | null) => void;
  collaborators: string[];
  onCollaborators: (emails: string[]) => void;
  onImages: (files: File[]) => void;
  archived: boolean;
  onArchive: () => void;
  /** The "More" menu entries after "Add label". */
  more: MenuEntry[];
  labels?: { all: LabelInfo[]; selected: string[]; onToggle: (l: LabelInfo, on: boolean) => void; onCreate: (name: string) => void };
  class?: string;
}

type Open = "color" | "reminder" | "collab" | "more" | "labels" | null;

/** Keep's note toolbar: palette, reminder, collaborator, image, archive, more. The same on cards, in the composer and in the dialog. */
export function NoteToolbar(props: ToolbarProps) {
  const [open, setOpen] = createSignal<Open>(null);
  let colorBtn: HTMLButtonElement | undefined;
  let remindBtn: HTMLButtonElement | undefined;
  let collabBtn: HTMLButtonElement | undefined;
  let moreBtn: HTMLButtonElement | undefined;
  let file: HTMLInputElement | undefined;
  const toggle = (what: Exclude<Open, null>) => setOpen(open() === what ? null : what);
  const close = () => setOpen(null);

  const entries = (): MenuEntry[] => {
    const out: MenuEntry[] = [];
    if (props.labels) {
      out.push({ id: "labels", label: props.labels.selected.length ? t("keep.changeLabels") : t("keep.addLabel"), run: () => setOpen("labels") });
    }
    return [...out, ...props.more];
  };

  return (
    <div class={props.class ? `kp-tools ${props.class}` : "kp-tools"} role="toolbar" aria-label={t("keep.more")} onClick={(e) => e.stopPropagation()}>
      <button ref={colorBtn} type="button" class="kp-icon-btn" data-tool="color" title={t("keep.background")} aria-label={t("keep.background")} aria-haspopup="dialog" onClick={() => toggle("color")}>
        <PaletteIcon />
      </button>
      <button ref={remindBtn} type="button" class="kp-icon-btn" data-tool="reminder" title={t("keep.remindMe")} aria-label={t("keep.remindMe")} aria-haspopup="dialog" onClick={() => toggle("reminder")}>
        <AddAlertIcon />
      </button>
      <button ref={collabBtn} type="button" class="kp-icon-btn" data-tool="collab" title={t("keep.collaborator")} aria-label={t("keep.collaborator")} aria-haspopup="dialog" onClick={() => toggle("collab")}>
        <PersonAddIcon />
      </button>
      <button type="button" class="kp-icon-btn" data-tool="image" title={t("keep.addImage")} aria-label={t("keep.addImage")} onClick={() => file?.click()}>
        <ImageIcon />
      </button>
      <input
        ref={file}
        type="file"
        accept={ALLOWED_IMAGE_TYPES.join(",")}
        multiple
        hidden
        data-testid="keep-image-input"
        onChange={(e) => {
          const files = [...(e.currentTarget.files ?? [])];
          e.currentTarget.value = "";
          if (files.length) props.onImages(files);
        }}
      />
      <button type="button" class="kp-icon-btn" data-tool="archive" title={props.archived ? t("keep.unarchiveAct") : t("keep.archiveAct")} aria-label={props.archived ? t("keep.unarchiveAct") : t("keep.archiveAct")} onClick={() => props.onArchive()}>
        {props.archived ? <UnarchiveIcon /> : <ArchiveIcon />}
      </button>
      <button ref={moreBtn} type="button" class="kp-icon-btn" data-tool="more" title={t("keep.more")} aria-label={t("keep.more")} aria-haspopup="menu" onClick={() => toggle("more")}>
        <MoreIcon />
      </button>
      <Switch>
        <Match when={open() === "color"}>
          <ColorPopover anchor={colorBtn} value={props.color} theme={props.theme} onPick={props.onColor} onClose={close} />
        </Match>
        <Match when={open() === "reminder"}>
          <ReminderPopover anchor={remindBtn} value={props.reminder} onSave={props.onReminder} onClose={close} />
        </Match>
        <Match when={open() === "collab"}>
          <CollaboratorPopover anchor={collabBtn} value={props.collaborators} onChange={props.onCollaborators} onClose={close} />
        </Match>
        <Match when={open() === "more"}>
          <MenuPopover anchor={moreBtn} label={t("keep.more")} entries={entries()} onClose={close} />
        </Match>
        <Match when={open() === "labels" && props.labels}>
          {(l) => <LabelPopover anchor={moreBtn} all={l().all} selected={l().selected} onToggle={l().onToggle} onCreate={l().onCreate} onClose={close} />}
        </Match>
      </Switch>
    </div>
  );
}
