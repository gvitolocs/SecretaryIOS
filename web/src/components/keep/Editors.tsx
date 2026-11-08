import { createMemo, For, Show, type JSX } from "solid-js";
import { attachmentUrl, isAudio, isImage, newItemId, type Attachment, type ChecklistItem } from "../../api/keep";
import { t } from "../../i18n";
import { ChecklistEditor } from "./ChecklistEditor";
import { FormatBar } from "./FormatBar";
import { CloseIcon } from "./icons";
import { RichEditor } from "./RichEditor";

/** The body of a note being written: title, then text or checklist, with the formatting bar for text. */
export function NoteFields(props: {
  title: string;
  onTitle: (v: string) => void;
  content: string;
  onContent: (v: string) => void;
  items: ChecklistItem[] | undefined;
  onItems: (items: ChecklistItem[]) => void;
  resetKey?: unknown;
  readOnly?: boolean;
  autofocusBody?: boolean;
  titleId?: string;
  showFormat: boolean;
  titleSlot?: JSX.Element;
}) {
  return (
    <>
      <div class="kp-title-row">
        <input
          id={props.titleId}
          class="kp-title-input"
          type="text"
          value={props.title}
          placeholder={t("keep.titlePh")}
          aria-label={t("keep.titlePh")}
          maxlength={200}
          readOnly={props.readOnly}
          autocomplete="off"
          onInput={(e) => props.onTitle(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || (e.key === "Tab" && !e.shiftKey)) {
              e.preventDefault();
              const target = e.currentTarget.closest(".kp-editor")?.querySelector<HTMLElement>(".kp-rich, .kp-item-text, .kp-item-add");
              target?.focus();
            }
          }}
        />
        {props.titleSlot}
      </div>
      <Show
        when={props.items}
        fallback={
          <>
            <RichEditor value={props.content} resetKey={props.resetKey} placeholder={t("keep.takeNote")} label={t("keep.takeNote")} readOnly={props.readOnly} autofocus={props.autofocusBody} onChange={props.onContent} />
            <Show when={props.showFormat && !props.readOnly}>
              <FormatBar />
            </Show>
          </>
        }
      >
        {(items) => <ChecklistEditor items={items()} onChange={props.onItems} readOnly={props.readOnly} autofocus={props.autofocusBody} />}
      </Show>
    </>
  );
}

/** Images of a note being edited, full width at the top, each with a delete button. */
export function EditorImages(props: { attachments: Attachment[]; uploading?: number; onDelete?: (a: Attachment) => void; readOnly?: boolean }) {
  const images = createMemo(() => props.attachments.filter(isImage));
  const audios = createMemo(() => props.attachments.filter(isAudio));
  return (
    <>
      <Show when={images().length > 0 || (props.uploading ?? 0) > 0}>
        <div class="kp-editor-images" data-count={Math.min(images().length, 4)}>
          <For each={images()}>
            {(a) => (
              <div class="kp-editor-image">
                <img src={attachmentUrl(a)} alt={a.name ?? ""} decoding="async" />
                <Show when={!props.readOnly && props.onDelete}>
                  <button type="button" class="kp-image-del" title={t("keep.deleteImage")} aria-label={t("keep.deleteImage")} onClick={() => props.onDelete?.(a)}>
                    <CloseIcon />
                  </button>
                </Show>
              </div>
            )}
          </For>
          <Show when={(props.uploading ?? 0) > 0}>
            <div class="kp-editor-image kp-editor-image--busy" role="status">
              {t("keep.uploading")}
            </div>
          </Show>
        </div>
      </Show>
      <For each={audios()}>
        {(a) => (
          <div class="kp-editor-audio">
            <audio controls preload="none" src={attachmentUrl(a)} aria-label={t("keep.audio")} />
          </div>
        )}
      </For>
    </>
  );
}

export const newBlankItem = (): ChecklistItem => ({ id: newItemId(), text: "", checked: false, indent: 0 });
