import { createEffect, createSignal, For, Show } from "solid-js";
import { toApiError } from "../../api/client";
import { notebooksApi, type NoteLink } from "../../api/notebooks";
import { plainExcerpt } from "../../api/notes";
import { PlusIcon, TrashIcon } from "../../components/Icons";
import { Button, Card, EmptyState, ListRow, RowList, Sheet } from "../../components/page";
import { t } from "../../i18n";
import { NotePicker } from "./NotePicker";

/** The Keep notes linked to a notebook: open one in Keep, write a new one, link or unlink. */
export function NotesColumn(props: {
  notebookId: string;
  notes: NoteLink[];
  onChanged: () => void;
  onError: (message: string) => void;
}) {
  const [writing, setWriting] = createSignal(false);
  const [linking, setLinking] = createSignal(false);
  const [title, setTitle] = createSignal("");
  const [text, setText] = createSignal("");
  const [slug, setSlug] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  createEffect(() => {
    if (!writing() && !linking()) return;
    setTitle("");
    setText("");
    setSlug("");
    setError(null);
    setBusy(false);
  });

  async function run(f: () => Promise<unknown>, close: (open: boolean) => void) {
    setBusy(true);
    setError(null);
    try {
      await f();
      close(false);
      props.onChanged();
    } catch (e) {
      setError(toApiError(e).message);
    } finally {
      setBusy(false);
    }
  }

  async function unlink(n: NoteLink) {
    try {
      await notebooksApi.unlinkNote(props.notebookId, n.slug);
    } catch (e) {
      props.onError(toApiError(e).message);
    }
    props.onChanged();
  }

  return (
    <Card
      list
      class="nb-card"
      title={t("notebooks.notes")}
      actions={
        <>
          <Button variant="ghost" onClick={() => setLinking(true)}>
            {t("notebooks.linkNote")}
          </Button>
          <Button variant="ghost" onClick={() => setWriting(true)}>
            <PlusIcon /> {t("notebooks.writeNote")}
          </Button>
        </>
      }
    >
      <Show when={props.notes.length} fallback={<EmptyState text={t("notebooks.notesEmpty")} />}>
        <RowList label={t("notebooks.notes")}>
          <For each={props.notes}>
            {(n) => (
              <ListRow
                class="nb-note"
                href={`/keep/note/${n.slug}`}
                title={n.title || n.slug}
                sub={plainExcerpt(n.excerpt, n.title) || n.slug}
                trailing={
                  <button
                    type="button"
                    class="pg-icon-btn"
                    aria-label={t("notebooks.unlinkNote", { title: n.title || n.slug })}
                    title={t("notebooks.unlinkNoteHint")}
                    onClick={() => void unlink(n)}
                  >
                    <TrashIcon />
                  </button>
                }
              />
            )}
          </For>
        </RowList>
      </Show>

      <Sheet
        open={writing()}
        onClose={() => setWriting(false)}
        title={t("notebooks.writeNote")}
        class="nb-sheet"
        footer={
          <>
            <Button variant="ghost" onClick={() => setWriting(false)}>
              {t("common.cancel")}
            </Button>
            <Button
              type="submit"
              form="nb-note-form"
              disabled={busy() || !title().trim() || !text().trim()}
            >
              {busy() ? t("common.saving") : t("notebooks.saveNote")}
            </Button>
          </>
        }
      >
        <form
          id="nb-note-form"
          class="nb-form"
          onSubmit={(e) => {
            e.preventDefault();
            if (title().trim() && text().trim() && !busy()) void run(() => notebooksApi.writeNote(props.notebookId, title().trim(), text()), setWriting);
          }}
        >
          <Show when={error()}>
            <p class="pg-notice" role="alert">{error()}</p>
          </Show>
          <label class="pg-field">
            <span class="pg-field-label">{t("notebooks.noteTitle")}</span>
            <input class="pg-input" value={title()} maxLength={200} onInput={(e) => setTitle(e.currentTarget.value)} />
          </label>
          <label class="pg-field">
            <span class="pg-field-label">{t("notebooks.noteText")}</span>
            <textarea class="pg-input nb-textarea" rows={9} value={text()} onInput={(e) => setText(e.currentTarget.value)} />
          </label>
          <p class="pg-muted nb-hint">{t("notebooks.noteHint")}</p>
        </form>
      </Sheet>

      <Sheet
        open={linking()}
        onClose={() => setLinking(false)}
        title={t("notebooks.linkNote")}
        class="nb-sheet"
        footer={
          <>
            <Button variant="ghost" onClick={() => setLinking(false)}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" form="nb-link-form" disabled={busy() || !slug()}>
              {busy() ? t("common.saving") : t("notebooks.link")}
            </Button>
          </>
        }
      >
        <form
          id="nb-link-form"
          class="nb-form"
          onSubmit={(e) => {
            e.preventDefault();
            if (slug() && !busy()) void run(() => notebooksApi.linkNote(props.notebookId, slug()), setLinking);
          }}
        >
          <Show when={error()}>
            <p class="pg-notice" role="alert">{error()}</p>
          </Show>
          <NotePicker value={slug()} onChange={setSlug} exclude={props.notes.map((n) => n.slug)} />
        </form>
      </Sheet>
    </Card>
  );
}
