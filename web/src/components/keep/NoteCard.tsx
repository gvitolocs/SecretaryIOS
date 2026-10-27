import { createMemo, For, Show } from "solid-js";
import { attachmentUrl, isAudio, isImage, type ChecklistItem, type KeepNote } from "../../api/keep";
import { t } from "../../i18n";
import { cardBackground, cardBorder } from "../../lib/keepColors";
import { hostOf } from "../../lib/keepText";
import { reminderLabel } from "../../lib/keepTime";
import { keep } from "../../state/keep";
import { archive, copyNote, deleteForever, restore, setColor, setPinned, trash, unarchive } from "../../state/keepActions";
import { selection, toggleSelected } from "../../state/keepUi";
import { useKeep } from "./ctx";
import { AudioIcon, CheckBoxIcon, CheckboxOutlineIcon, CheckCircleIcon, ClockIcon, DeleteForeverIcon, LinkIcon, PinFilledIcon, PinOutlineIcon, RestoreIcon } from "./icons";
import { KeepText } from "./KeepText";
import { NoteToolbar } from "./NoteToolbar";

/** Keep clips a card's list at 20 unchecked rows. */
export const CARD_LIST_MAX = 20;

export function labelsConfig(id: string, tags: () => string[]) {
  return {
    all: keep.labels(),
    selected: tags(),
    onToggle: (l: { tag: string; name: string }, on: boolean) => void (on ? keep.addLabel([id], l.name) : keep.removeLabel([id], l.tag)),
    onCreate: (name: string) => void keep.addLabel([id], name),
  };
}

export function CardImages(props: { note: KeepNote; max?: number }) {
  const images = createMemo(() => props.note.attachments.filter(isImage));
  const shown = () => images().slice(0, props.max ?? 4);
  return (
    <Show when={images().length > 0}>
      <div class="kp-images" data-count={shown().length}>
        <For each={shown()}>
          {(a, i) => (
            <div class="kp-image">
              <img src={attachmentUrl(a)} alt="" loading="lazy" decoding="async" draggable={false} />
              <Show when={i() === shown().length - 1 && images().length > shown().length}>
                <span class="kp-image-more">{t("keep.imagesMore", { count: images().length - shown().length })}</span>
              </Show>
            </div>
          )}
        </For>
      </div>
    </Show>
  );
}

function CardList(props: { note: KeepNote; readOnly: boolean }) {
  const open = createMemo(() => (props.note.items ?? []).filter((i) => !i.checked));
  const doneCount = createMemo(() => (props.note.items ?? []).filter((i) => i.checked).length);
  const shown = () => open().slice(0, CARD_LIST_MAX);
  const toggle = (item: ChecklistItem) => {
    if (props.readOnly) return;
    void keep.update(props.note.id, { items: (props.note.items ?? []).map((i) => (i.id === item.id ? { ...i, checked: !i.checked } : i)) });
  };
  return (
    <div class="kp-card-list">
      <For each={shown()}>
        {(item) => (
          <div class="kp-card-item" data-indent={item.indent}>
            <button
              type="button"
              class="kp-card-item-box"
              role="checkbox"
              aria-checked={item.checked}
              aria-label={item.text}
              tabindex={-1}
              onClick={(e) => {
                e.stopPropagation();
                toggle(item);
              }}
            >
              <CheckboxOutlineIcon />
            </button>
            <span class="kp-card-item-text">{item.text}</span>
          </div>
        )}
      </For>
      <Show when={open().length > shown().length}>
        <div class="kp-card-more">{t("keep.moreItems", { count: open().length - shown().length })}</div>
      </Show>
      <Show when={doneCount() > 0}>
        <div class="kp-card-more kp-card-more--checked">
          <CheckBoxIcon />
          <span>{t("keep.checkedMore", { count: doneCount() })}</span>
        </div>
      </Show>
    </div>
  );
}

export function NoteCard(props: { note: KeepNote; draggable?: boolean; onDragStart?: (e: DragEvent) => void; onDragEnd?: () => void }) {
  const ctx = useKeep();
  const n = () => props.note;
  const selected = () => selection().has(n().id);
  const inTrash = () => !!n().deletedAt;
  const anySelected = () => selection().size > 0;
  const hasText = () => (n().items ? false : n().content.trim() !== "");
  const bg = () => cardBackground(n().color, ctx.theme());
  const border = () => cardBorder(n().color, ctx.theme());
  const audios = createMemo(() => n().attachments.filter(isAudio));
  const hasChips = () => !!n().reminder || n().annotations.length > 0 || n().tags.length > 0 || n().collaborators.length > 0 || audios().length > 0;

  return (
    <article
      class="kp-card"
      classList={{ "kp-card--selected": selected(), "kp-card--pinned": n().pinned, "kp-card--trash": inTrash(), "kp-card--tmp": n().id.startsWith("tmp_") }}
      data-note={n().id}
      data-color={n().color}
      tabindex="0"
      aria-label={n().title || t("keep.openNote")}
      draggable={props.draggable}
      onDragStart={(e) => props.onDragStart?.(e)}
      onDragEnd={() => props.onDragEnd?.()}
      style={{ "--kp-card-bg": bg(), "--kp-card-border": border() }}
      onClick={() => (anySelected() ? toggleSelected(n().id) : ctx.open(n().id))}
      onKeyDown={(e) => {
        if (e.target !== e.currentTarget) return;
        if (e.key === "Enter") ctx.open(n().id);
        else if (e.key === " ") {
          e.preventDefault();
          toggleSelected(n().id);
        }
      }}
    >
      <button
        type="button"
        class="kp-select"
        aria-pressed={selected()}
        aria-label={selected() ? t("keep.deselect") : t("keep.select")}
        title={selected() ? t("keep.deselect") : t("keep.select")}
        onClick={(e) => {
          e.stopPropagation();
          toggleSelected(n().id);
        }}
      >
        <CheckCircleIcon />
      </button>
      <CardImages note={n()} />
      <div class="kp-card-body">
        <Show when={!inTrash()}>
          <button
            type="button"
            class="kp-pin"
            classList={{ "kp-pin--on": n().pinned }}
            aria-pressed={n().pinned}
            aria-label={n().pinned ? t("keep.unpin") : t("keep.pin")}
            title={n().pinned ? t("keep.unpin") : t("keep.pin")}
            onClick={(e) => {
              e.stopPropagation();
              setPinned([n().id], !n().pinned);
            }}
          >
            {n().pinned ? <PinFilledIcon /> : <PinOutlineIcon />}
          </button>
        </Show>
        <Show when={n().title}>
          <div class="kp-card-title">{n().title}</div>
        </Show>
        <Show when={n().items}>
          <CardList note={n()} readOnly={inTrash()} />
        </Show>
        <Show when={hasText()}>
          <KeepText class="kp-card-text" text={n().content} />
        </Show>
        <Show when={hasChips()}>
          <div class="kp-chips">
            <Show when={n().reminder}>
              {(r) => (
                <span class="kp-chip kp-chip--reminder" title={t("keep.remindMe")}>
                  <ClockIcon />
                  <span>{reminderLabel(r().at)}</span>
                </span>
              )}
            </Show>
            <For each={audios()}>
              {() => (
                <span class="kp-chip">
                  <AudioIcon />
                  <span>{t("keep.audio")}</span>
                </span>
              )}
            </For>
            <For each={n().annotations}>
              {(a) => (
                <a class="kp-chip kp-chip--link" href={a.url} target="_blank" rel="noopener noreferrer nofollow" title={a.url} onClick={(e) => e.stopPropagation()}>
                  <LinkIcon />
                  <span>{a.title || hostOf(a.url)}</span>
                </a>
              )}
            </For>
            <For each={n().tags}>
              {(tag) => (
                <button
                  type="button"
                  class="kp-chip kp-chip--label"
                  data-label={tag}
                  onClick={(e) => {
                    e.stopPropagation();
                    ctx.goLabel(tag);
                  }}
                >
                  {keep.labelName(tag)}
                </button>
              )}
            </For>
            <For each={n().collaborators}>
              {(email) => (
                <span class="kp-avatar kp-avatar--sm" title={email} aria-label={email}>
                  {email.charAt(0).toUpperCase()}
                </span>
              )}
            </For>
          </div>
        </Show>
      </div>
      <div class="kp-card-foot">
        <Show
          when={!inTrash()}
          fallback={
            <div class="kp-tools">
              <button
                type="button"
                class="kp-icon-btn"
                data-tool="delete-forever"
                title={t("keep.deleteForever")}
                aria-label={t("keep.deleteForever")}
                onClick={(e) => {
                  e.stopPropagation();
                  deleteForever([n().id]);
                }}
              >
                <DeleteForeverIcon />
              </button>
              <button
                type="button"
                class="kp-icon-btn"
                data-tool="restore"
                title={t("keep.restore")}
                aria-label={t("keep.restore")}
                onClick={(e) => {
                  e.stopPropagation();
                  restore([n().id]);
                }}
              >
                <RestoreIcon />
              </button>
            </div>
          }
        >
          <NoteToolbar
            theme={ctx.theme()}
            color={n().color}
            onColor={(c) => setColor([n().id], c)}
            reminder={n().reminder}
            onReminder={(r) => void keep.update(n().id, { reminder: r })}
            collaborators={n().collaborators}
            onCollaborators={(emails) => void keep.update(n().id, { collaborators: emails })}
            onImages={(files) => void keep.uploadImages(n().id, files)}
            archived={n().archived}
            onArchive={() => (n().archived ? unarchive([n().id]) : archive([n().id]))}
            labels={labelsConfig(n().id, () => n().tags)}
            more={[
              { id: "delete", label: t("keep.deleteNote"), run: () => trash([n().id]) },
              { id: "copy", label: t("keep.makeCopy"), run: () => void copyNote(n().id) },
            ]}
          />
        </Show>
      </div>
    </article>
  );
}
