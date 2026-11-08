import { createSignal, For, Match, Show, Switch } from "solid-js";
import { toApiError } from "../api/client";
import { importGoogleKeep, type ImportReport } from "../api/keep";
import { t } from "../i18n";
import { keep } from "../state/keep";

// Settings → Import → Google Keep: pick the Takeout .zip, "Check" (a dry run: nothing is written), then "Import".
// The zip is uploaded as the raw body of POST /v1/notes/import/google-keep (?dryRun=1 for the check).

type Phase = "idle" | "checking" | "checked" | "importing" | "done" | "error";

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

function Stat(props: { label: string; value: number }) {
  return (
    <Show when={props.value > 0}>
      <div class="kp-imp-stat">
        <dt>{props.label}</dt>
        <dd>{props.value}</dd>
      </div>
    </Show>
  );
}

export function ImportReportView(props: { report: ImportReport }) {
  const r = () => props.report;
  return (
    <div class="kp-imp-report" data-testid="import-report">
      <h4>{r().dryRun ? t("keep.importDry") : t("keep.importDone")}</h4>
      <dl class="kp-imp-stats">
        <div class="kp-imp-stat kp-imp-stat--main">
          <dt>{t("keep.importNotes")}</dt>
          <dd>{r().notes.total}</dd>
        </div>
        <Stat label={t("keep.importNew")} value={r().notes.new} />
        <Stat label={t("keep.importUpdated")} value={r().notes.updated} />
        <Stat label={t("keep.importUnchanged")} value={r().notes.unchanged} />
        <Stat label={t("keep.importKept")} value={r().notes.keptLocalEdits} />
        <Stat label={t("keep.importSkipped")} value={r().notes.skippedDeleted} />
        <Stat label={t("keep.importLists")} value={r().notes.lists} />
        <Stat label={t("keep.importPinned")} value={r().notes.pinned} />
        <Stat label={t("keep.importArchived")} value={r().notes.archived} />
        <Stat label={t("keep.importTrashed")} value={r().notes.trashed} />
      </dl>
      <p>{t("keep.importLabels", { total: r().labels.total, fresh: r().labels.new.length })}</p>
      <Show when={r().labels.new.length > 0}>
        <p class="muted small">{r().labels.new.join(", ")}</p>
      </Show>
      <p>{t("keep.importAttachments", { total: r().attachments.total, fresh: r().attachments.new, size: formatBytes(r().attachments.bytes) })}</p>
      <p class="muted small">{t("keep.importFiles", { json: r().files.json, attachments: r().files.attachments, html: r().files.html, ignored: r().files.ignored })}</p>
      <Show when={r().attachments.missing.length > 0}>
        <details class="kp-imp-warn">
          <summary>{t("keep.importMissing", { count: r().attachments.missing.length })}</summary>
          <ul>
            <For each={r().attachments.missing.slice(0, 20)}>{(f) => <li>{f}</li>}</For>
          </ul>
        </details>
      </Show>
      <Show when={r().errors.length > 0}>
        <details class="kp-imp-warn" open>
          <summary>{t("keep.importErrors", { count: r().errors.length })}</summary>
          <ul>
            <For each={r().errors.slice(0, 20)}>
              {(e) => (
                <li>
                  {e.file}: {e.error}
                </li>
              )}
            </For>
          </ul>
        </details>
      </Show>
      <Show when={r().sample.length > 0}>
        <details>
          <summary>{t("keep.importSample")}</summary>
          <ul class="kp-imp-sample">
            <For each={r().sample}>
              {(s) => (
                <li>
                  <strong>{s.title || "—"}</strong>
                  <span class="muted small">
                    {" "}
                    {s.kind === "list" ? t("keep.kindList") : t("keep.kindText")}
                    {s.labels.length ? ` · ${s.labels.join(", ")}` : ""}
                  </span>
                </li>
              )}
            </For>
          </ul>
        </details>
      </Show>
    </div>
  );
}

export function KeepImport(props: { desktop?: boolean; run?: typeof importGoogleKeep }) {
  const run = props.run ?? importGoogleKeep;
  const [file, setFile] = createSignal<File | null>(null);
  const [phase, setPhase] = createSignal<Phase>("idle");
  const [progress, setProgress] = createSignal(0);
  const [report, setReport] = createSignal<ImportReport | null>(null);
  const [error, setError] = createSignal("");
  let picker: HTMLInputElement | undefined;

  const busy = () => phase() === "checking" || phase() === "importing";
  const percent = () => `${Math.round(progress() * 100)}%`;

  async function go(dryRun: boolean) {
    const f = file();
    if (!f || busy()) return;
    setPhase(dryRun ? "checking" : "importing");
    setProgress(0);
    setError("");
    try {
      const r = await run(f, { dryRun, onProgress: setProgress });
      setReport(r);
      setPhase(dryRun ? "checked" : "done");
      if (!dryRun) void keep.sync();
    } catch (e) {
      const err = toApiError(e);
      setError(err.code === "import_running" ? t("keep.importRunning") : t("keep.importFailed", { message: err.message }));
      setPhase("error");
    }
  }

  function choose(f: File | undefined) {
    if (!f) return;
    if (!/\.zip$/i.test(f.name)) {
      setFile(null);
      setError(t("keep.importOnlyZip"));
      setPhase("error");
      return;
    }
    setFile(f);
    setReport(null);
    setError("");
    setPhase("idle");
  }

  return (
    <section class={props.desktop ? "pg-form" : "settings-form kp-import"} aria-labelledby={props.desktop ? undefined : "kp-import-title"} id="kp-import">
      <Show when={!props.desktop}><h3 class={props.desktop ? "pg-field-label" : "field-label"} id="kp-import-title">
        {t("keep.importTitle")}
      </h3></Show>
      <h4 class="kp-imp-sub">{t("keep.importKeepTitle")}</h4>
      <p class={props.desktop ? "pg-muted" : "muted small"}>{t("keep.importKeepIntro")}</p>
      <ol class="kp-imp-steps">
        <li>{t("keep.importStep1")}</li>
        <li>{t("keep.importStep2")}</li>
        <li>{t("keep.importStep3")}</li>
        <li>{t("keep.importStep4")}</li>
      </ol>
      <div class="kp-imp-pick">
        <input ref={picker} type="file" accept=".zip,application/zip" hidden data-testid="import-file" onChange={(e) => choose(e.currentTarget.files?.[0])} />
        <button type="button" class={props.desktop ? "pg-btn" : "btn"} disabled={busy()} onClick={() => picker?.click()}>
          {t("keep.importPick")}
        </button>
        <span class="muted small kp-imp-name">{file() ? `${file()!.name} (${formatBytes(file()!.size)})` : t("keep.importNoFile")}</span>
      </div>
      <div class="kp-imp-actions">
        <button type="button" class={props.desktop ? "pg-btn" : "btn"} data-action="check" disabled={!file() || busy()} onClick={() => void go(true)}>
          {t("keep.importCheck")}
        </button>
        <button type="button" class={props.desktop ? "pg-btn" : "btn btn--primary"} data-action="import" disabled={!file() || busy()} onClick={() => void go(false)}>
          {t("keep.importRun")}
        </button>
      </div>
      <Show when={busy()}>
        <div class="kp-imp-progress" role="status">
          <progress max="1" value={progress()} aria-label={percent()} />
          <span class={props.desktop ? "pg-muted" : "muted small"}>
            <Switch>
              <Match when={progress() >= 1}>{t("keep.importProcessing")}</Match>
              <Match when={true}>{phase() === "checking" ? t("keep.uploadingPercent", { percent: percent() }) : t("keep.importing", { percent: percent() })}</Match>
            </Switch>
          </span>
        </div>
      </Show>
      <Show when={error()}>
        <p class={props.desktop ? "pg-notice" : "form-error"} role="alert">
          {error()}
        </p>
      </Show>
      <Show when={report()}>{(r) => <ImportReportView report={r()} />}</Show>
      <Show when={phase() === "checked"}>
        <p class={props.desktop ? "pg-muted" : "muted small"}>{t("keep.importThenRun")}</p>
      </Show>
      <p class={props.desktop ? "pg-muted" : "muted small"}>{t("keep.importSafe")}</p>
    </section>
  );
}
