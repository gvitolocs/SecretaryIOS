import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import { GYM_MAX_INPUT_BYTES, REST_PRESETS, type GymView } from "../../api/gym";
import { dismissGymError, gym, runGym } from "../../state/gym";
import { GymPage } from "./shared";
import { gt } from "./strings";

// Settings (SettingsScreen.swift): training preferences, export / restore,
// and the About text. The engine produces the export texts and validates a
// restore; this screen only moves files in and out.


/** `Lucina-Gym-2026-10-21.json` / `.csv`, by the local calendar date. */
export function backupFileName(ext: "json" | "csv", date: Date = new Date()): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `Lucina-Gym-${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}.${ext}`;
}

/** Hand the browser a text file to save. Started only by a user's click. */
export function downloadText(text: string, fileName: string, type: string): void {
  const url = URL.createObjectURL(new Blob([text], { type }));
  const a = document.createElement("a");
  a.href = url;
  a.download = fileName;
  a.rel = "noopener";
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1_000);
}

function readFileText(file: File): Promise<string> {
  if (typeof file.text === "function") return file.text();
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result ?? ""));
    reader.onerror = () => reject(reader.error ?? new Error("read failed"));
    reader.readAsText(file);
  });
}

/** Bytes the backup takes inside the request envelope (JSON-escaped, UTF-8). */
function envelopeBytes(text: string): number {
  return new TextEncoder().encode(JSON.stringify(text)).length + 1_024;
}

type Status = { kind: "ok" | "error"; text: string } | null;

function Preferences(props: { view: GymView }) {
  const [unit, setUnit] = createSignal("kg");
  const [rest, setRest] = createSignal(90);
  const [goal, setGoal] = createSignal(3);
  const [saving, setSaving] = createSignal(false);
  const [saved, setSaved] = createSignal(false);

  createEffect(() => {
    const s = props.view.state.settings;
    setUnit(s.weight_unit);
    setRest(s.rest_seconds);
    setGoal(s.weekly_goal);
  });

  // A rest time set elsewhere (e.g. 75 s) stays selectable instead of being replaced.
  const restOptions = createMemo(() => {
    const current = props.view.state.settings.rest_seconds;
    const presets: number[] = [...REST_PRESETS];
    return presets.includes(current) ? presets : [...presets, current].sort((a, b) => a - b);
  });

  async function save(e: SubmitEvent) {
    e.preventDefault();
    if (saving()) return;
    setSaving(true);
    setSaved(false);
    try {
      const res = await runGym({ type: "update_settings", weight_unit: unit(), rest_seconds: rest(), weekly_goal: goal() });
      if (res?.ok) setSaved(true);
    } finally {
      setSaving(false);
    }
  }

  return (
    <form class="card gym-settings-card" onSubmit={save} aria-labelledby="gym-prefs-title">
      <h2 id="gym-prefs-title" class="gym-eyebrow">
        {gt("trainingPreferences")}
      </h2>
      <fieldset class="segmented-field">
        <legend class="field-label">{gt("weightUnit")}</legend>
        <div class="segmented">
          <For each={[{ value: "kg", label: gt("kilograms") }, { value: "lb", label: gt("pounds") }]}>
            {(o) => (
              <label class="segment" classList={{ "segment--on": unit() === o.value }}>
                <input
                  type="radio"
                  name="gym-unit"
                  class="sr-only"
                  value={o.value}
                  checked={unit() === o.value}
                  onChange={() => setUnit(o.value)}
                />
                {o.label}
              </label>
            )}
          </For>
        </div>
      </fieldset>
      <fieldset class="segmented-field">
        <legend class="field-label">{gt("restTimer")}</legend>
        <div class="segmented segmented--wrap">
          <For each={restOptions()}>
            {(seconds) => (
              <label class="segment" classList={{ "segment--on": rest() === seconds }}>
                <input
                  type="radio"
                  name="gym-rest"
                  class="sr-only"
                  value={seconds}
                  checked={rest() === seconds}
                  onChange={() => setRest(seconds)}
                />
                {gt("restOption", { seconds })}
              </label>
            )}
          </For>
        </div>
      </fieldset>
      <div class="stepper" role="group" aria-labelledby="gym-goal-label">
        <span id="gym-goal-label" class="stepper-label">
          {gt("weeklyGoal", { count: goal() })}
        </span>
        <button
          type="button"
          class="icon-btn stepper-btn"
          aria-label={gt("fewerWorkouts")}
          disabled={goal() <= 1}
          onClick={() => setGoal((g) => Math.max(1, g - 1))}
        >
          −
        </button>
        <button
          type="button"
          class="icon-btn stepper-btn"
          aria-label={gt("moreWorkouts")}
          disabled={goal() >= 14}
          onClick={() => setGoal((g) => Math.min(14, g + 1))}
        >
          +
        </button>
      </div>
      <button type="submit" class="btn btn--primary btn--block gym-primary" disabled={saving()}>
        {gt("savePreferences")}
      </button>
      <Show when={saved()}>
        <p class="muted small" role="status">
          {gt("preferencesSaved")}
        </p>
      </Show>
    </form>
  );
}

function YourData() {
  const [exporting, setExporting] = createSignal<"json" | "csv" | null>(null);
  const [pending, setPending] = createSignal<{ name: string; text: string } | null>(null);
  const [restoring, setRestoring] = createSignal(false);
  const [status, setStatus] = createSignal<Status>(null);
  let fileInput: HTMLInputElement | undefined;
  let restoreButton: HTMLButtonElement | undefined;

  async function exportAs(ext: "json" | "csv") {
    if (exporting()) return;
    setExporting(ext);
    setStatus(null);
    try {
      const res = await runGym({ type: ext === "json" ? "export_json" : "export_csv" });
      if (!res?.ok || typeof res.text !== "string") {
        if (!gym.actionError) setStatus({ kind: "error", text: gt("exportFailed") });
        return;
      }
      const fileName = backupFileName(ext);
      downloadText(res.text, fileName, ext === "json" ? "application/json" : "text/csv");
      setStatus({ kind: "ok", text: gt("exported", { file: fileName }) });
    } finally {
      setExporting(null);
    }
  }

  async function chooseFile(file: File | undefined) {
    if (!file) return;
    setStatus(null);
    setPending(null);
    if (file.size > GYM_MAX_INPUT_BYTES) {
      setStatus({ kind: "error", text: gt("backupTooLarge") });
      return;
    }
    let text: string;
    try {
      text = await readFileText(file);
    } catch {
      setStatus({ kind: "error", text: gt("fileUnreadable") });
      return;
    }
    if (envelopeBytes(text) > GYM_MAX_INPUT_BYTES) {
      setStatus({ kind: "error", text: gt("backupTooLarge") });
      return;
    }
    try {
      JSON.parse(text);
    } catch {
      setStatus({ kind: "error", text: gt("notABackup") });
      return;
    }
    setPending({ name: file.name, text });
  }

  async function restore() {
    const backup = pending();
    if (!backup || restoring()) return;
    setRestoring(true);
    try {
      const res = await runGym({ type: "restore_json", backup: backup.text });
      setPending(null);
      if (res?.ok) {
        setStatus({ kind: "ok", text: gt("restored", { file: backup.name }) });
      } else {
        // The engine's reason ("Invalid backup: …") is shown here, not twice.
        const reason = res?.error ?? gym.actionError ?? gt("restoreFailed");
        dismissGymError();
        setStatus({ kind: "error", text: reason });
      }
    } finally {
      setRestoring(false);
      restoreButton?.focus();
    }
  }

  return (
    <section class="card gym-settings-card" aria-labelledby="gym-data-title">
      <h2 id="gym-data-title" class="gym-eyebrow">
        {gt("yourData")}
      </h2>
      <div class="data-actions">
        <button type="button" class="btn btn--block" disabled={exporting() !== null} onClick={() => void exportAs("json")}>
          {gt("exportJson")}
        </button>
        <button type="button" class="btn btn--block" disabled={exporting() !== null} onClick={() => void exportAs("csv")}>
          {gt("exportCsv")}
        </button>
        <button
          type="button"
          class="btn btn--block"
          ref={restoreButton}
          disabled={restoring()}
          onClick={() => fileInput?.click()}
        >
          {gt("restoreJson")}
        </button>
        <input
          ref={fileInput}
          type="file"
          class="sr-only"
          accept="application/json,.json"
          aria-label={gt("backupFile")}
          tabindex="-1"
          onChange={(e) => {
            const input = e.currentTarget;
            const file = input.files?.[0];
            input.value = "";
            void chooseFile(file);
          }}
        />
      </div>
      <Show when={pending()}>
        {(backup) => (
          <div class="restore-confirm" role="alertdialog" aria-labelledby="restore-confirm-title" aria-describedby="restore-confirm-file">
            <p id="restore-confirm-title" class="restore-confirm-title">
              {gt("restoreQuestion")}
            </p>
            <p id="restore-confirm-file" class="muted small">
              {gt("restoreDetail", { file: backup().name })}
            </p>
            <div class="confirm-pair">
              <button type="button" class="btn btn--small btn--danger" disabled={restoring()} onClick={() => void restore()}>
                {gt("restoreBackup")}
              </button>
              <button type="button" class="btn btn--small" disabled={restoring()} onClick={() => setPending(null)}>
                {gt("cancel")}
              </button>
            </div>
          </div>
        )}
      </Show>
      <Show when={status()}>
        {(s) => (
          <p class={s().kind === "error" ? "field-error" : "muted small"} role={s().kind === "error" ? "alert" : "status"}>
            {s().text}
          </p>
        )}
      </Show>
    </section>
  );
}

export function SettingsBody(props: { view: GymView }) {
  return (
    <>
      <h1 class="view-title gym-title">{gt("settingsTitle")}</h1>
      <Preferences view={props.view} />
      <YourData />
      <section class="card gym-settings-card" aria-label={gt("about")}>
        <p class="gym-about-title">{gt("aboutTitle")}</p>
        <p class="muted small">{gt("aboutText")}</p>
      </section>
    </>
  );
}

/** Stand-alone route component, for when the router gets a /gym/settings path. */
export default function GymSettingsView() {
  return <GymPage>{(view) => <SettingsBody view={view()} />}</GymPage>;
}
