import { createEffect, createMemo, createResource, createSignal, For, on, Show } from "solid-js";
import { agentsApi, type DiffFile, type DiffMode, type FileContent, type FileEntry } from "../../api/agents";
import { toApiError } from "../../api/client";
import { t } from "../../i18n";
import { baseName } from "../../lib/agentsTimeline";

function lineNumbers(file: DiffFile) {
  const rows: { type: string; content: string; n: string }[] = [];
  for (const h of file.hunks) {
    rows.push({ type: "hunk", content: `@@ -${h.oldStart},${h.oldCount} +${h.newStart},${h.newCount} @@`, n: "" });
    let o = h.oldStart;
    let n = h.newStart;
    for (const l of h.lines) {
      if (l.type === "add") rows.push({ type: "add", content: l.content, n: String(n++) });
      else if (l.type === "remove") rows.push({ type: "remove", content: l.content, n: String(o++) });
      else if (l.type === "context") {
        rows.push({ type: "context", content: l.content, n: String(n) });
        o++;
        n++;
      } else rows.push({ type: "header", content: l.content, n: "" });
    }
  }
  return rows;
}

function fileState(f: DiffFile): string {
  return f.isNew ? t("agents.diffNew") : f.isDeleted ? t("agents.diffDeleted") : "";
}

/** A workspace's changes: the file list and a readable unified diff, in the three modes agentd offers. */
export function DiffPane(props: { workspaceId: string }) {
  const [mode, setMode] = createSignal<DiffMode>("all");
  const [ignoreWs, setIgnoreWs] = createSignal(false);
  const [selected, setSelected] = createSignal<string | null>(null);
  const [diff, { refetch }] = createResource(
    () => ({ id: props.workspaceId, mode: mode(), ws: ignoreWs() }),
    (k) => agentsApi.diff(k.id, k.mode, k.ws),
  );
  const d = () => (diff.error ? undefined : diff());
  const files = () => d()?.files ?? [];
  const current = createMemo(() => files().find((f) => f.path === selected()) ?? files()[0]);
  createEffect(on(() => props.workspaceId, () => setSelected(null)));
  const totals = () => files().reduce((a, f) => ({ add: a.add + f.additions, del: a.del + f.deletions }), { add: 0, del: 0 });
  const MODES: { id: DiffMode; label: () => string }[] = [
    { id: "all", label: () => t("agents.diffAll") },
    { id: "uncommitted", label: () => t("agents.diffUncommitted") },
    { id: "base", label: () => t("agents.diffBase") },
  ];

  return (
    <div class="ag-diffpane">
      <div class="ag-toolbar">
        <div class="ag-seg" role="group" aria-label={t("agents.diffMode")}>
          <For each={MODES}>
            {(m) => (
              <button type="button" aria-pressed={mode() === m.id} onClick={() => setMode(m.id)}>
                {m.label()}
              </button>
            )}
          </For>
        </div>
        <label class="ag-check">
          <input type="checkbox" checked={ignoreWs()} onChange={(e) => setIgnoreWs(e.currentTarget.checked)} /> {t("agents.ignoreWhitespace")}
        </label>
        <button type="button" class="ag-btn ag-btn--small" onClick={() => void refetch()}>{t("agents.refresh")}</button>
        <Show when={files().length > 0}>
          <span class="ag-muted ag-toolbar__sum">
            {t("agents.diffSummary", { count: files().length })} <span class="ag-add">+{totals().add}</span> <span class="ag-del">−{totals().del}</span>
          </span>
        </Show>
      </div>
      <Show when={diff.error}>
        <p class="ag-note" role="alert">{toApiError(diff.error).message}</p>
      </Show>
      <Show when={d()?.error}>
        <p class="ag-note" role="alert">{d()!.error}</p>
      </Show>
      <Show when={diff.loading && !d()}>
        <p class="ag-muted" role="status">{t("agents.loading")}</p>
      </Show>
      <Show when={d() && !diff.error && files().length === 0}>
        <p class="ag-muted">{t("agents.noChanges")}</p>
      </Show>
      <Show when={d()?.diffTooLarge}>
        <p class="ag-note">{t("agents.diffTooLarge")}</p>
      </Show>
      <Show when={files().length > 0}>
        <div class="ag-diffgrid">
          <ul class="ag-filelist" aria-label={t("agents.changedFiles")}>
            <For each={files()}>
              {(f) => (
                <li>
                  <button type="button" data-on={current()?.path === f.path ? "yes" : "no"} aria-current={current()?.path === f.path ? "true" : undefined} title={f.path} onClick={() => setSelected(f.path)}>
                    <span class="ag-filelist__name">{baseName(f.path)}</span>
                    <span class="ag-filelist__dir">{f.path.includes("/") ? f.path.slice(0, f.path.lastIndexOf("/")) : ""}</span>
                    <span class="ag-filelist__n"><span class="ag-add">+{f.additions}</span> <span class="ag-del">−{f.deletions}</span></span>
                  </button>
                </li>
              )}
            </For>
          </ul>
          <Show when={current()}>
            {(f) => (
              <section class="ag-diff" aria-label={f().path}>
                <h3 class="ag-diff__path">
                  {f().path}
                  <Show when={f().oldPath && f().oldPath !== f().path}><span class="ag-muted"> ← {f().oldPath}</span></Show>
                  <Show when={fileState(f())}><span class="ag-tag">{fileState(f())}</span></Show>
                </h3>
                <Show when={f().status === "ok"} fallback={<p class="ag-muted">{f().status === "binary" ? t("agents.diffBinary") : t("agents.diffFileTooLarge")}</p>}>
                  <div class="ag-diff__code" role="table" aria-label={t("agents.diff")}>
                    <For each={lineNumbers(f())}>
                      {(l) => (
                        <div class="ag-dl" role="row" data-t={l.type}>
                          <span class="ag-dl__n" role="cell">{l.n}</span>
                          <span class="ag-dl__sign" aria-hidden="true">{l.type === "add" ? "+" : l.type === "remove" ? "−" : ""}</span>
                          <span class="ag-dl__c" role="cell">{l.content}</span>
                        </div>
                      )}
                    </For>
                  </div>
                </Show>
              </section>
            )}
          </Show>
        </div>
      </Show>
    </div>
  );
}

const parentOf = (path: string): string => (path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "");

function sizeText(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** A workspace's files: browse folders, read a text file, download any file through a one-use link. */
export function FilesPane(props: { workspaceId: string }) {
  const [dir, setDir] = createSignal("");
  const [open, setOpen] = createSignal<string | null>(null);
  const [listing] = createResource(
    () => ({ id: props.workspaceId, dir: dir() }),
    (k) => agentsApi.files(k.id, k.dir),
  );
  const [file, { refetch: refetchFile }] = createResource(
    () => (open() ? { id: props.workspaceId, path: open()! } : null),
    (k) => agentsApi.file(k.id, k.path),
  );
  const [note, setNote] = createSignal<string | null>(null);
  createEffect(on(() => props.workspaceId, () => { setDir(""); setOpen(null); }));

  const lst = () => (listing.error ? undefined : listing());
  const fl = () => (file.error ? undefined : file());
  const entries = (): FileEntry[] =>
    [...(lst()?.entries ?? [])].sort((a, b) => (a.kind === b.kind ? a.name.localeCompare(b.name) : a.kind === "directory" ? -1 : 1));

  async function download(f: FileContent) {
    setNote(null);
    try {
      const url = await agentsApi.downloadUrl(props.workspaceId, f.path);
      const a = document.createElement("a");
      a.href = url;
      a.download = baseName(f.path);
      document.body.append(a);
      a.click();
      a.remove();
    } catch (e) {
      setNote(toApiError(e).message);
    }
  }

  return (
    <div class="ag-filespane" data-open={open() ? "yes" : "no"}>
      <div class="ag-files">
        <p class="ag-crumb">
          <button type="button" onClick={() => setDir("")}>/</button>
          <Show when={dir()}>
            <span>{dir()}</span>
            <button type="button" class="ag-btn ag-btn--small" onClick={() => setDir(parentOf(dir()))}>{t("agents.up")}</button>
          </Show>
        </p>
        <Show when={listing.error}>
          <p class="ag-note" role="alert">{toApiError(listing.error).message}</p>
        </Show>
        <Show when={listing.loading && !lst()}>
          <p class="ag-muted" role="status">{t("agents.loading")}</p>
        </Show>
        <ul class="ag-filelist ag-filelist--tree" aria-label={t("agents.files")}>
          <For each={entries()}>
            {(e) => (
              <li>
                <button
                  type="button"
                  data-on={open() === e.path ? "yes" : "no"}
                  onClick={() => (e.kind === "directory" ? (setDir(e.path), setOpen(null)) : setOpen(e.path))}
                >
                  <span class="ag-filelist__name">{e.kind === "directory" ? `${e.name}/` : e.name}</span>
                  <span class="ag-filelist__n ag-muted">{e.kind === "directory" ? "" : sizeText(e.size)}</span>
                </button>
              </li>
            )}
          </For>
        </ul>
        <Show when={lst() && entries().length === 0}>
          <p class="ag-muted">{t("agents.emptyFolder")}</p>
        </Show>
      </div>
      <Show when={open()}>
        <section class="ag-viewer" aria-label={baseName(open()!)}>
          <header class="ag-viewer__head">
            <button type="button" class="ag-btn ag-btn--small ag-viewer__back" onClick={() => setOpen(null)}>{t("agents.backToFiles")}</button>
            <h3>{open()}</h3>
            <Show when={fl()}>
              <button type="button" class="ag-btn ag-btn--small" onClick={() => void download(fl()!)}>{t("agents.download")}</button>
            </Show>
          </header>
          <Show when={file.error}>
            <p class="ag-note" role="alert">{toApiError(file.error).message} <button type="button" class="ag-link" onClick={() => void refetchFile()}>{t("agents.retry")}</button></p>
          </Show>
          <Show when={note()}>
            <p class="ag-note" role="alert">{note()}</p>
          </Show>
          <Show when={file.loading && !file()}>
            <p class="ag-muted" role="status">{t("agents.loading")}</p>
          </Show>
          <Show when={fl()}>
            {(f) => (
              <Show when={f().content !== undefined && f().encoding !== "base64"} fallback={<p class="ag-muted">{t("agents.binaryFile", { size: sizeText(f().size) })}</p>}>
                <pre class="ag-code ag-viewer__text">{f().content}</pre>
              </Show>
            )}
          </Show>
        </section>
      </Show>
    </div>
  );
}
