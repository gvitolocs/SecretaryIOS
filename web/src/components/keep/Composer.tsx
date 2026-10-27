import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { ALLOWED_IMAGE_TYPES, normLabel, type ChecklistItem, type KeepColor } from "../../api/keep";
import { t } from "../../i18n";
import { cardBackground, cardBorder } from "../../lib/keepColors";
import { draftToInput, emptyDraft, type Draft } from "../../lib/keepDraft";
import { reminderLabel } from "../../lib/keepTime";
import { contentToItems, keep } from "../../state/keep";
import { useKeep } from "./ctx";
import { NoteFields, newBlankItem } from "./Editors";
import { CheckBoxIcon, ClockIcon, CloseIcon, ImageIcon, PinFilledIcon, PinOutlineIcon, TextFormatIcon } from "./icons";
import { NoteToolbar } from "./NoteToolbar";

/** Save a closed draft as a note (an empty one is discarded). Returns the new note's temporary id. */
export function saveDraft(d: Draft): string | null {
  const input = draftToInput(d);
  if (!input) return null;
  const made = keep.create(input);
  if (d.files.length) void made.saved.then((n) => n && keep.uploadImages(n.id, d.files));
  return made.id;
}

/** Keep's "Take a note…" card: one line until clicked, then title + text (or list) with the note toolbar. */
export function Composer(props: { initialLabel?: string | undefined }) {
  const ctx = useKeep();
  const [open, setOpen] = createSignal(false);
  const [draft, setDraft] = createSignal<Draft>(emptyDraft());
  const [showFormat, setShowFormat] = createSignal(false);
  const [resetKey, setResetKey] = createSignal(0);
  let card: HTMLDivElement | undefined;
  let imageInput: HTMLInputElement | undefined;

  const patch = (p: Partial<Draft>) => setDraft({ ...draft(), ...p });
  const previews = createMemo(() => draft().files.map((f) => ({ name: f.name, url: URL.createObjectURL(f) })), [], { equals: false });
  createEffect(on(previews, (_, prev) => prev?.forEach((p) => URL.revokeObjectURL(p.url)), { defer: true }));
  onCleanup(() => previews().forEach((p) => URL.revokeObjectURL(p.url)));

  function start(kind: "text" | "list", files: File[] = []) {
    setDraft({ ...emptyDraft(), ...(kind === "list" ? { items: [newBlankItem()] } : {}), ...(props.initialLabel ? { labels: [props.initialLabel] } : {}), files });
    setShowFormat(false);
    setResetKey(resetKey() + 1);
    setOpen(true);
  }

  function finish(save: boolean) {
    if (!open()) return;
    const d = draft();
    setOpen(false);
    if (save) saveDraft(d);
    setDraft(emptyDraft());
  }

  // A press outside the open card closes it, saving the note.
  onMount(() => {
    const outside = (e: PointerEvent) => {
      if (!open() || !card) return;
      const target = e.target as Node | null;
      if (target && (card.contains(target) || (target as HTMLElement).closest?.(".kp-pop"))) return;
      finish(true);
    };
    document.addEventListener("pointerdown", outside, true);
    onCleanup(() => document.removeEventListener("pointerdown", outside, true));
  });

  const toggleList = () => {
    const d = draft();
    if (d.items) patch({ items: undefined, content: d.items.map((i) => i.text).join("\n") });
    else {
      const items = contentToItems(d.content);
      patch({ items: items.length ? items : [newBlankItem()], content: "" });
    }
  };

  const labelNames = () => draft().labels;
  const allLabels = () => keep.labels();

  return (
    <div class="kp-composer-wrap" classList={{ "kp-composer-wrap--open": open() }}>
      <Show
        when={open()}
        fallback={
          <div class="kp-composer kp-composer--collapsed" role="group" aria-label={t("keep.takeNote")}>
            <button type="button" class="kp-composer-ph" onClick={() => start("text")}>
              {t("keep.takeNote")}
            </button>
            <button type="button" class="kp-icon-btn" data-tool="new-list" title={t("keep.newList")} aria-label={t("keep.newList")} onClick={() => start("list")}>
              <CheckBoxIcon />
            </button>
            <button type="button" class="kp-icon-btn" data-tool="new-image" title={t("keep.newImage")} aria-label={t("keep.newImage")} onClick={() => imageInput?.click()}>
              <ImageIcon />
            </button>
            <input
              ref={imageInput}
              type="file"
              hidden
              multiple
              accept={ALLOWED_IMAGE_TYPES.join(",")}
              onChange={(e) => {
                const files = [...(e.currentTarget.files ?? [])];
                e.currentTarget.value = "";
                if (files.length) start("text", files);
              }}
            />
          </div>
        }
      >
        <div
          ref={card}
          class="kp-composer kp-composer--open kp-editor"
          data-color={draft().color}
          style={{ "--kp-card-bg": cardBackground(draft().color, ctx.theme()), "--kp-card-border": cardBorder(draft().color, ctx.theme()) }}
          role="group"
          aria-label={t("keep.newNoteLabel")}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.stopPropagation();
              finish(true);
            }
          }}
        >
          <Show when={previews().length > 0}>
            <div class="kp-editor-images" data-count={Math.min(previews().length, 4)}>
              <For each={previews()}>
                {(p, i) => (
                  <div class="kp-editor-image">
                    <img src={p.url} alt={p.name} />
                    <button
                      type="button"
                      class="kp-image-del"
                      title={t("keep.deleteImage")}
                      aria-label={t("keep.deleteImage")}
                      onClick={() => patch({ files: draft().files.filter((_, k) => k !== i()) })}
                    >
                      <CloseIcon />
                    </button>
                  </div>
                )}
              </For>
            </div>
          </Show>
          <NoteFields
            title={draft().title}
            onTitle={(title) => patch({ title })}
            content={draft().content}
            onContent={(content) => patch({ content })}
            items={draft().items}
            onItems={(items: ChecklistItem[]) => patch({ items })}
            resetKey={resetKey()}
            autofocusBody
            showFormat={showFormat()}
            titleSlot={
              <button type="button" class="kp-pin kp-pin--inline" classList={{ "kp-pin--on": draft().pinned }} aria-pressed={draft().pinned} aria-label={draft().pinned ? t("keep.unpin") : t("keep.pin")} title={draft().pinned ? t("keep.unpin") : t("keep.pin")} onClick={() => patch({ pinned: !draft().pinned })}>
                {draft().pinned ? <PinFilledIcon /> : <PinOutlineIcon />}
              </button>
            }
          />
          <Show when={labelNames().length > 0 || draft().reminder || draft().collaborators.length > 0}>
            <div class="kp-chips">
              <Show when={draft().reminder}>
                {(r) => (
                  <span class="kp-chip kp-chip--reminder">
                    <ClockIcon />
                    <span>{reminderLabel(r().at)}</span>
                    <button type="button" class="kp-chip-x" aria-label={t("keep.deleteReminder")} onClick={() => patch({ reminder: undefined as never })}>
                      <CloseIcon />
                    </button>
                  </span>
                )}
              </Show>
              <For each={labelNames()}>
                {(name) => (
                  <span class="kp-chip kp-chip--label">
                    {name}
                    <button type="button" class="kp-chip-x" aria-label={t("keep.removeLabel", { name })} onClick={() => patch({ labels: labelNames().filter((l) => l !== name) })}>
                      <CloseIcon />
                    </button>
                  </span>
                )}
              </For>
              <For each={draft().collaborators}>
                {(email) => (
                  <span class="kp-avatar kp-avatar--sm" title={email}>
                    {email.charAt(0).toUpperCase()}
                  </span>
                )}
              </For>
            </div>
          </Show>
          <div class="kp-editor-foot">
            <Show when={!draft().items}>
              <button type="button" class="kp-icon-btn" data-tool="format" aria-pressed={showFormat()} title={t("keep.formatting")} aria-label={t("keep.formatting")} onClick={() => setShowFormat(!showFormat())}>
                <TextFormatIcon />
              </button>
            </Show>
            <NoteToolbar
              theme={ctx.theme()}
              color={draft().color}
              onColor={(color: KeepColor) => patch({ color })}
              reminder={draft().reminder}
              onReminder={(r) => (r ? patch({ reminder: r }) : setDraft({ ...draft(), reminder: undefined as never }))}
              collaborators={draft().collaborators}
              onCollaborators={(collaborators) => patch({ collaborators })}
              onImages={(files) => patch({ files: [...draft().files, ...files] })}
              archived={draft().archived}
              onArchive={() => patch({ archived: !draft().archived })}
              labels={{
                all: allLabels(),
                selected: labelNames().map(normLabel),
                onToggle: (l, on) => patch({ labels: on ? [...labelNames(), l.name] : labelNames().filter((n) => normLabel(n) !== l.tag) }),
                onCreate: (name) => patch({ labels: [...labelNames(), name] }),
              }}
              more={[
                { id: "delete", label: t("keep.deleteNote"), run: () => finish(false) },
                { id: "checkboxes", label: draft().items ? t("keep.hideCheckboxes") : t("keep.showCheckboxes"), run: toggleList },
              ]}
            />
            <button type="button" class="kp-close-btn" onClick={() => finish(true)}>
              {t("keep.close")}
            </button>
          </div>
        </div>
      </Show>
    </div>
  );
}
