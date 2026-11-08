import { createEffect, createSignal, Show } from "solid-js";
import { toApiError } from "../../api/client";
import { notebooksApi, type NewSource } from "../../api/notebooks";
import { Button, SectionNav, Sheet } from "../../components/page";
import { t } from "../../i18n";
import { NotePicker } from "./NotePicker";

type Mode = "url" | "text" | "file" | "sandbox" | "note";

const MAX_UPLOAD = 200 * 1024 * 1024;

/**
 * The "Add source" sheet: a link (web page or YouTube), pasted text, an uploaded file, a path on
 * Lucina's computer, or one of the owner's Keep notes.
 */
export function AddSourceSheet(props: { open: boolean; notebookId: string; onClose: () => void; onAdded: () => void }) {
  const [mode, setMode] = createSignal<Mode>("url");
  const [url, setUrl] = createSignal("");
  const [title, setTitle] = createSignal("");
  const [text, setText] = createSignal("");
  const [path, setPath] = createSignal("");
  const [slug, setSlug] = createSignal("");
  const [file, setFile] = createSignal<File | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  // A fresh form each time the sheet opens.
  createEffect(() => {
    if (!props.open) return;
    setMode("url");
    setUrl("");
    setTitle("");
    setText("");
    setPath("");
    setSlug("");
    setFile(null);
    setError(null);
    setBusy(false);
  });

  const valid = () => {
    switch (mode()) {
      case "url":
        return /^https?:\/\/\S+$/i.test(url().trim());
      case "text":
        return text().trim().length > 0;
      case "file":
        return !!file();
      case "sandbox":
        return path().trim().startsWith("/");
      case "note":
        return !!slug();
    }
  };

  async function submit(e: Event) {
    e.preventDefault();
    if (!valid() || busy()) return;
    setBusy(true);
    setError(null);
    try {
      const id = props.notebookId;
      if (mode() === "file") {
        const f = file()!;
        if (f.size > MAX_UPLOAD) throw new Error(t("notebooks.tooBig"));
        await notebooksApi.uploadSource(id, f, title().trim() || undefined);
      } else {
        const body: NewSource =
          mode() === "url"
            ? { kind: "url", url: url().trim() }
            : mode() === "text"
              ? { kind: "text", ...(title().trim() ? { title: title().trim() } : {}), text: text() }
              : mode() === "sandbox"
                ? { kind: "sandbox", path: path().trim() }
                : { kind: "note", slug: slug() };
        await notebooksApi.addSource(id, body);
      }
      props.onAdded();
      props.onClose();
    } catch (err) {
      setError(toApiError(err).message);
    } finally {
      setBusy(false);
    }
  }

  const modes: { id: Mode; label: () => string }[] = [
    { id: "url", label: () => t("notebooks.addLink") },
    { id: "text", label: () => t("notebooks.addText") },
    { id: "file", label: () => t("notebooks.addFile") },
    { id: "sandbox", label: () => t("notebooks.addPath") },
    { id: "note", label: () => t("notebooks.addNote") },
  ];

  return (
    <Sheet
      open={props.open}
      onClose={props.onClose}
      title={t("notebooks.addSource")}
      class="nb-sheet"
      tools={
        <SectionNav
          label={t("notebooks.addSource")}
          items={modes.map((m) => ({ label: m.label(), onSelect: () => setMode(m.id), current: mode() === m.id }))}
        />
      }
      footer={
        <>
          <Button variant="ghost" onClick={props.onClose}>
            {t("common.cancel")}
          </Button>
          <Button type="submit" form="nb-source-form" disabled={!valid() || busy()}>
            {busy() ? t("notebooks.adding") : t("notebooks.addSourceButton")}
          </Button>
        </>
      }
    >
      <form id="nb-source-form" class="nb-form" onSubmit={submit}>
        <Show when={error()}>
          <p class="pg-notice" role="alert">{error()}</p>
        </Show>
        <Show when={mode() === "url"}>
          <div class="nb-fieldgroup">
            <label class="pg-field">
              <span class="pg-field-label">{t("notebooks.urlLabel")}</span>
              <input class="pg-input" type="url" placeholder="https://" aria-describedby="nb-url-hint" value={url()} onInput={(e) => setUrl(e.currentTarget.value)} />
            </label>
            <span class="pg-muted nb-hint" id="nb-url-hint">{t("notebooks.linkHint")}</span>
          </div>
        </Show>
        <Show when={mode() === "text"}>
          <label class="pg-field">
            <span class="pg-field-label">{t("notebooks.sourceTitle")}</span>
            <input class="pg-input" value={title()} maxLength={200} onInput={(e) => setTitle(e.currentTarget.value)} />
          </label>
          <label class="pg-field">
            <span class="pg-field-label">{t("notebooks.pastedText")}</span>
            <textarea class="pg-input nb-textarea" rows={8} value={text()} onInput={(e) => setText(e.currentTarget.value)} />
          </label>
        </Show>
        <Show when={mode() === "file"}>
          <div class="nb-fieldgroup">
            <label class="pg-field">
              <span class="pg-field-label">{t("notebooks.file")}</span>
              <input class="pg-input nb-file" type="file" aria-describedby="nb-file-hint" onChange={(e) => setFile(e.currentTarget.files?.[0] ?? null)} />
            </label>
            <span class="pg-muted nb-hint" id="nb-file-hint">{t("notebooks.fileHint")}</span>
          </div>
          <label class="pg-field">
            <span class="pg-field-label">{t("notebooks.sourceTitle")}</span>
            <input class="pg-input" value={title()} maxLength={200} onInput={(e) => setTitle(e.currentTarget.value)} />
          </label>
        </Show>
        <Show when={mode() === "sandbox"}>
          <div class="nb-fieldgroup">
            <label class="pg-field">
              <span class="pg-field-label">{t("notebooks.path")}</span>
              <input class="pg-input" placeholder="/home/cua/Documents/notes.pdf" aria-describedby="nb-path-hint" value={path()} onInput={(e) => setPath(e.currentTarget.value)} />
            </label>
            <span class="pg-muted nb-hint" id="nb-path-hint">{t("notebooks.pathHint")}</span>
          </div>
        </Show>
        <Show when={mode() === "note"}>
          <div class="pg-field">
            <span class="pg-field-label">{t("notebooks.keepNote")}</span>
            <NotePicker value={slug()} onChange={setSlug} />
          </div>
        </Show>
      </form>
    </Sheet>
  );
}
