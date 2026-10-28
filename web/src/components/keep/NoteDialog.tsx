import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import type { ChecklistItem, NoteInput } from "../../api/keep";
import { t } from "../../i18n";
import { cardBackground, cardBorder } from "../../lib/keepColors";
import { editedLabel, reminderLabel } from "../../lib/keepTime";
import { contentToItems, itemsToContent, keep } from "../../state/keep";
import { archive, copyNote, deleteForever, restore, setColor, setPinned, trash, unarchive } from "../../state/keepActions";
import { useKeep } from "./ctx";
import { EditorImages, NoteFields } from "./Editors";
import { CloseIcon, ClockIcon, DeleteForeverIcon, LinkIcon, PinFilledIcon, PinOutlineIcon, RestoreIcon, TextFormatIcon } from "./icons";
import { labelsConfig } from "./NoteCard";
import { NoteToolbar } from "./NoteToolbar";
import { hostOf, splitTitleHeading } from "../../lib/keepText";

const SAVE_DELAY_MS = 600;

function DialogBody(props: { id: string; onClose: () => void }) {
  const ctx = useKeep();
  const n = () => keep.note(props.id)!;
  const readOnly = () => !!n().deletedAt;
  const [title, setTitle] = createSignal(n().title);
  // The "# <title>" line some notes start with is not shown (it repeats the title) but is kept when saving.
  const split = splitTitleHeading(n().content, n().title);
  let head = split.head;
  const [content, setContent] = createSignal(split.body);
  const [items, setItems] = createSignal<ChecklistItem[] | undefined>(n().items?.map((i) => ({ ...i })));
  const [resetKey, setResetKey] = createSignal(0);
  const [showFormat, setShowFormat] = createSignal(false);
  let dirty: { title?: true; body?: true } = {};
  let timer: ReturnType<typeof setTimeout> | undefined;
  let scrim: HTMLDivElement | undefined;

  function flush() {
    clearTimeout(timer);
    if (!dirty.title && !dirty.body) return;
    const input: NoteInput = {};
    if (dirty.title) input.title = title().trim();
    if (dirty.body) {
      const it = items();
      if (it) input.items = it;
      else input.content = (head + content()).replace(/\s+$/, "");
    }
    dirty = {};
    void keep.update(props.id, input);
  }
  const touch = (what: "title" | "body") => {
    dirty[what] = true;
    clearTimeout(timer);
    timer = setTimeout(flush, SAVE_DELAY_MS);
  };
  onCleanup(flush);

  const close = () => {
    flush();
    props.onClose();
  };

  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !e.defaultPrevented) close();
    };
    document.addEventListener("keydown", onKey);
    onCleanup(() => document.removeEventListener("keydown", onKey));
    scrim?.querySelector<HTMLElement>(readOnly() ? ".kp-close-btn" : n().content || n().items ? ".kp-rich, .kp-item-text" : ".kp-title-input")?.focus({ preventScroll: true });
  });

  function toggleCheckboxes() {
    flush();
    const it = items();
    if (it) {
      const text = it.map((i) => i.text).join("\n");
      setItems(undefined);
      setContent(text);
      setResetKey(resetKey() + 1);
      void keep.update(props.id, { convert: "text" });
    } else {
      const next = contentToItems(head + content());
      head = "";
      const list = next.length ? next : [{ id: `i_${Math.random().toString(16).slice(2, 14).padEnd(12, "0")}`, text: "", checked: false, indent: 0 as const }];
      setItems(list);
      setContent(itemsToContent(list));
      void keep.update(props.id, { items: list });
    }
  }

  const checkedCount = createMemo(() => (items() ?? []).filter((i) => i.checked).length);
  const bulk = (fn: (list: ChecklistItem[]) => ChecklistItem[]) => {
    const list = fn(items() ?? []);
    setItems(list);
    setContent(itemsToContent(list));
    touch("body");
  };

  return (
    <div
      ref={scrim}
      class="kp-scrim"
      onPointerDown={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div
        class="kp-dialog kp-editor"
        role="dialog"
        aria-modal="true"
        aria-label={n().title || t("keep.openNote")}
        data-color={n().color}
        data-note={n().id}
        style={{ "--kp-card-bg": cardBackground(n().color, ctx.theme()), "--kp-card-border": cardBorder(n().color, ctx.theme()) }}
      >
        <EditorImages
          attachments={n().attachments}
          uploading={keep.uploading()[props.id]}
          readOnly={readOnly()}
          onDelete={(a) => void keep.removeAttachment(props.id, a.id)}
        />
        <div class="kp-dialog-scroll">
          <Show when={readOnly()}>
            <p class="kp-banner">{t("keep.cantEditTrash")}</p>
          </Show>
          <NoteFields
            title={title()}
            onTitle={(v) => {
              setTitle(v);
              touch("title");
            }}
            content={content()}
            onContent={(v) => {
              setContent(v);
              touch("body");
            }}
            items={items()}
            onItems={(list) => {
              setItems(list);
              setContent(itemsToContent(list));
              touch("body");
            }}
            resetKey={resetKey()}
            readOnly={readOnly()}
            showFormat={showFormat()}
            titleSlot={
              <Show when={!readOnly()}>
                <button type="button" class="kp-pin kp-pin--inline" classList={{ "kp-pin--on": n().pinned }} aria-pressed={n().pinned} aria-label={n().pinned ? t("keep.unpin") : t("keep.pin")} title={n().pinned ? t("keep.unpin") : t("keep.pin")} onClick={() => setPinned([props.id], !n().pinned)}>
                  {n().pinned ? <PinFilledIcon /> : <PinOutlineIcon />}
                </button>
              </Show>
            }
          />
          <Show when={n().reminder || n().tags.length > 0 || n().annotations.length > 0 || n().collaborators.length > 0}>
            <div class="kp-chips">
              <Show when={n().reminder}>
                {(r) => (
                  <span class="kp-chip kp-chip--reminder">
                    <ClockIcon />
                    <span>{reminderLabel(r().at)}</span>
                    <Show when={!readOnly()}>
                      <button type="button" class="kp-chip-x" aria-label={t("keep.deleteReminder")} onClick={() => void keep.update(props.id, { reminder: null })}>
                        <CloseIcon />
                      </button>
                    </Show>
                  </span>
                )}
              </Show>
              <For each={n().annotations}>
                {(a) => (
                  <a class="kp-chip kp-chip--link" href={a.url} target="_blank" rel="noopener noreferrer nofollow" title={a.url}>
                    <LinkIcon />
                    <span>{a.title || hostOf(a.url)}</span>
                  </a>
                )}
              </For>
              <For each={n().tags}>
                {(tag) => (
                  <span class="kp-chip kp-chip--label" data-label={tag}>
                    {keep.labelName(tag)}
                    <Show when={!readOnly()}>
                      <button type="button" class="kp-chip-x" aria-label={t("keep.removeLabel", { name: keep.labelName(tag) })} onClick={() => void keep.removeLabel([props.id], tag)}>
                        <CloseIcon />
                      </button>
                    </Show>
                  </span>
                )}
              </For>
              <For each={n().collaborators}>
                {(email) => (
                  <span class="kp-avatar kp-avatar--sm" title={email}>
                    {email.charAt(0).toUpperCase()}
                  </span>
                )}
              </For>
            </div>
          </Show>
        </div>
        <div class="kp-dialog-meta">{t("keep.edited", { time: editedLabel(n().updatedAt) })}</div>
        <div class="kp-editor-foot">
          <Show
            when={!readOnly()}
            fallback={
              <div class="kp-tools">
                <button type="button" class="kp-icon-btn" data-tool="delete-forever" title={t("keep.deleteForever")} aria-label={t("keep.deleteForever")} onClick={() => { deleteForever([props.id]); props.onClose(); }}>
                  <DeleteForeverIcon />
                </button>
                <button type="button" class="kp-icon-btn" data-tool="restore" title={t("keep.restore")} aria-label={t("keep.restore")} onClick={() => { restore([props.id]); props.onClose(); }}>
                  <RestoreIcon />
                </button>
              </div>
            }
          >
            <Show when={!items()}>
              <button type="button" class="kp-icon-btn" data-tool="format" aria-pressed={showFormat()} title={t("keep.formatting")} aria-label={t("keep.formatting")} onClick={() => setShowFormat(!showFormat())}>
                <TextFormatIcon />
              </button>
            </Show>
            <NoteToolbar
              theme={ctx.theme()}
              color={n().color}
              onColor={(c) => setColor([props.id], c)}
              reminder={n().reminder}
              onReminder={(r) => void keep.update(props.id, { reminder: r })}
              collaborators={n().collaborators}
              onCollaborators={(emails) => void keep.update(props.id, { collaborators: emails })}
              onImages={(files) => void keep.uploadImages(props.id, files)}
              archived={n().archived}
              onArchive={() => {
                flush();
                if (n().archived) unarchive([props.id]);
                else archive([props.id]);
                props.onClose();
              }}
              labels={labelsConfig(props.id, () => n().tags)}
              more={[
                {
                  id: "delete",
                  label: t("keep.deleteNote"),
                  run: () => {
                    flush();
                    trash([props.id]);
                    props.onClose();
                  },
                },
                { id: "copy", label: t("keep.makeCopy"), run: () => void copyNote(props.id) },
                { id: "checkboxes", label: items() ? t("keep.hideCheckboxes") : t("keep.showCheckboxes"), run: toggleCheckboxes },
                ...(items() && checkedCount() > 0
                  ? [
                      { id: "uncheck", label: t("keep.uncheckAll"), run: () => bulk((l) => l.map((i) => ({ ...i, checked: false }))) },
                      { id: "delete-checked", label: t("keep.deleteChecked"), run: () => bulk((l) => l.filter((i) => !i.checked)) },
                    ]
                  : []),
              ]}
            />
          </Show>
          <button type="button" class="kp-close-btn" onClick={close}>
            {t("keep.close")}
          </button>
        </div>
      </div>
    </div>
  );
}

/** Keep's centred note dialog, in the note's colour. Edits save a moment after typing and on close. */
export function NoteDialog(props: { id: string; onClose: () => void }) {
  const note = () => keep.note(props.id);
  return (
    <Show
      when={note()}
      fallback={
        <Show when={keep.loaded() && keep.status() !== "syncing" && !keep.resolving()}>
          <div class="kp-scrim" onPointerDown={(e) => e.target === e.currentTarget && props.onClose()}>
            <div class="kp-dialog kp-dialog--gone" role="alertdialog" aria-label={t("keep.noteGone")}>
              <p>{t("keep.noteGone")}</p>
              <button type="button" class="kp-close-btn" onClick={props.onClose}>
                {t("keep.close")}
              </button>
            </div>
          </div>
        </Show>
      }
    >
      <DialogBody id={props.id} onClose={props.onClose} />
    </Show>
  );
}
