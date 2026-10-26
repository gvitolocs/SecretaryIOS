import { A } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, onMount, Show } from "solid-js";
import { parseWeightOnServer, type BodyEntry, type BodyStatLabel, type BodyView } from "../api/body";
import { toApiError } from "../api/client";
import { type ChartSeries, LineChart } from "../components/LineChart";
import { t } from "../i18n";
import { readString, writeString } from "../lib/storage";
import { formatDate, formatDateTime, formatWeight, weightValue, type WeightUnit } from "../lib/units";
import { body, dismissBodyError, refreshBody, runBody } from "../state/body";
import { currentTimeZone } from "../state/engine";

// The body engine owns parsing, statistics and labels (docs/server-api.md,
// "Body module"). This view sends text, shows the engine's labels, and only
// converts grams to the display unit to place points on the chart.

const WEIGHT_UNIT_KEY = "secretary.weightUnit";
const DASH = "—";

function localDateTime(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function localDateTimeMs(text: string): number | null {
  if (!text) return null;
  const ms = new Date(text).getTime();
  return Number.isFinite(ms) ? ms : null;
}

function statLabel(view: BodyView | null, key: BodyStatLabel): string {
  return view?.stat_labels[key] ?? DASH;
}

function EntryRow(props: { entry: BodyEntry; unit: WeightUnit; label: string | undefined }) {
  const [editing, setEditing] = createSignal(false);
  const [weight, setWeight] = createSignal("");
  const [when, setWhen] = createSignal("");
  const [note, setNote] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);
  const [pending, setPending] = createSignal(false);
  const [confirmDelete, setConfirmDelete] = createSignal(false);

  function startEdit() {
    setWeight(formatWeight(props.entry.weight_grams, props.unit));
    setWhen(localDateTime(props.entry.measured_ms));
    setNote(props.entry.note);
    setError(null);
    setConfirmDelete(false);
    setEditing(true);
  }

  async function save(e: SubmitEvent) {
    e.preventDefault();
    if (pending()) return;
    const measured = localDateTimeMs(when());
    if (measured === null) {
      setError(t("weight.invalidDate"));
      return;
    }
    // Only send what changed; omitted fields keep their value.
    const changes: { weight?: string; measured_ms?: number; note?: string } = {};
    if (weight().trim() !== formatWeight(props.entry.weight_grams, props.unit)) changes.weight = weight().trim();
    if (when() !== localDateTime(props.entry.measured_ms)) changes.measured_ms = measured;
    if (note().trim() !== props.entry.note) changes.note = note().trim();
    if (Object.keys(changes).length === 0) {
      setEditing(false);
      return;
    }
    setError(null);
    setPending(true);
    try {
      const result = await runBody({ type: "edit_entry", entry_id: props.entry.id, ...changes });
      if (result?.ok) setEditing(false);
      else if (result?.error) setError(result.error);
    } finally {
      setPending(false);
    }
  }

  async function remove() {
    if (pending()) return;
    setPending(true);
    try {
      await runBody({ type: "delete_entry", entry_id: props.entry.id });
    } finally {
      setPending(false);
    }
  }

  return (
    <li class="weight-row" classList={{ "weight-row--pending": pending() }}>
      <Show
        when={!editing()}
        fallback={
          <form class="weight-edit" onSubmit={(e) => void save(e)} aria-label={t("weight.editForm")}>
            <label class="field-label" for={`edit-weight-${props.entry.id}`}>
              {t("weight.weightIn", { unit: props.unit })}
            </label>
            <input
              id={`edit-weight-${props.entry.id}`}
              class="text-input"
              type="text"
              inputmode="decimal"
              value={weight()}
              onInput={(e) => setWeight(e.currentTarget.value)}
              autocomplete="off"
              disabled={pending()}
            />
            <label class="field-label" for={`edit-when-${props.entry.id}`}>
              {t("weight.when")}
            </label>
            <input
              id={`edit-when-${props.entry.id}`}
              class="text-input"
              type="datetime-local"
              value={when()}
              onInput={(e) => setWhen(e.currentTarget.value)}
              disabled={pending()}
            />
            <label class="field-label" for={`edit-note-${props.entry.id}`}>
              {t("weight.note")}
            </label>
            <input
              id={`edit-note-${props.entry.id}`}
              class="text-input"
              type="text"
              value={note()}
              onInput={(e) => setNote(e.currentTarget.value)}
              maxlength="1000"
              autocomplete="off"
              disabled={pending()}
            />
            <Show when={error()}>{(message) => <p class="form-error">{message()}</p>}</Show>
            <div class="weight-edit-actions">
              <button class="btn btn--small" type="button" onClick={() => setEditing(false)} disabled={pending()}>
                {t("common.cancel")}
              </button>
              <button class="btn btn--primary btn--small" type="submit" disabled={pending()}>
                {t("common.save")}
              </button>
            </div>
          </form>
        }
      >
        <div class="weight-row-main">
          <div class="weight-row-text">
            <span class="weight-row-weight">{props.label ?? DASH}</span>
            <span class="muted small">{formatDateTime(props.entry.measured_ms)}</span>
            <Show when={props.entry.note}>{(text) => <span class="weight-row-note">{text()}</span>}</Show>
          </div>
          <div class="weight-row-actions">
            <button class="btn btn--small" type="button" onClick={startEdit} disabled={pending()}>
              {t("common.edit")}
            </button>
            <Show
              when={confirmDelete()}
              fallback={
                <button class="btn btn--small" type="button" onClick={() => setConfirmDelete(true)} disabled={pending()}>
                  {t("common.delete")}
                </button>
              }
            >
              <button class="btn btn--small btn--danger" type="button" onClick={() => void remove()} disabled={pending()}>
                {t("common.confirmDelete")}
              </button>
              <button class="btn btn--small" type="button" onClick={() => setConfirmDelete(false)} disabled={pending()}>
                {t("common.cancel")}
              </button>
            </Show>
          </div>
        </div>
      </Show>
    </li>
  );
}

/** `compact`: the Training page's summary (log, stats, chart); the entries and settings stay on /weight. */
export default function WeightView(props: { compact?: boolean } = {}) {
  const stored = readString("local", WEIGHT_UNIT_KEY);
  const storedUnit: WeightUnit = stored === "kg" || stored === "lb" ? stored : "kg";
  let unitChosen = stored === "kg" || stored === "lb";

  const [unit, setUnit] = createSignal<WeightUnit>(storedUnit);
  const [weightText, setWeightText] = createSignal("");
  const [note, setNote] = createSignal("");
  const [logError, setLogError] = createSignal<string | null>(null);
  const [logPending, setLogPending] = createSignal(false);

  const [settingsUnit, setSettingsUnit] = createSignal<WeightUnit>("kg");
  const [goalText, setGoalText] = createSignal("");
  const [settingsError, setSettingsError] = createSignal<string | null>(null);
  const [settingsPending, setSettingsPending] = createSignal(false);

  onMount(() => void refreshBody());

  createEffect(() => {
    const view = body.view;
    if (view && !unitChosen) setUnit(view.settings.unit);
  });

  createEffect(() => {
    const view = body.view;
    if (!view) return;
    setSettingsUnit(view.settings.unit);
    setGoalText(view.settings.goal_grams === null ? "" : formatWeight(view.settings.goal_grams, view.settings.unit));
  });

  const viewUnit = (): WeightUnit => body.view?.settings.unit ?? "kg";
  const entries = () => body.view?.entries ?? [];

  const detailLine = createMemo(() => {
    const v = body.view;
    if (!v || v.stats.count === 0) return "";
    return t("weight.detail", {
      min: statLabel(v, "min"),
      max: statLabel(v, "max"),
      d30: statLabel(v, "trend30"),
      d90: statLabel(v, "trend90"),
    });
  });

  const goalValue = createMemo(() => {
    const v = body.view;
    if (!v || v.stats.goal_grams === null) return null;
    if (v.stats.goal_reached) return t("weight.goalReached");
    return statLabel(v, "goal_distance");
  });

  const chartData = createMemo<{ series: ChartSeries[]; labels: string[] }>(() => {
    const view = body.view;
    const points = view?.series ?? [];
    const u = viewUnit();
    const series: ChartSeries[] = [
      { key: "weight", points: points.map((p) => ({ x: p.measured_ms, y: weightValue(p.weight_grams, u) })), class: "chart-line--primary" },
      { key: "avg7", points: points.map((p) => ({ x: p.measured_ms, y: weightValue(p.avg7_grams, u) })), class: "chart-line--secondary" },
    ];
    const first = points[0];
    const last = points[points.length - 1];
    const labels = first && last && points.length > 1 ? [formatDate(first.measured_ms), formatDate(last.measured_ms)] : [];
    return { series, labels };
  });

  function chooseUnit(next: WeightUnit) {
    unitChosen = true;
    setUnit(next);
    writeString("local", WEIGHT_UNIT_KEY, next);
  }

  async function submitLog(e: SubmitEvent) {
    e.preventDefault();
    if (logPending()) return;
    const text = weightText().trim();
    if (!text) {
      setLogError(t("weight.enterWeight"));
      return;
    }
    const chosen = unit();
    const noteText = note().trim();
    setLogError(null);
    setLogPending(true);
    try {
      // The engine parses `weight` in the display unit; for the other unit ask it to parse first.
      let weight: { weight: string } | { weight_grams: number };
      if (chosen === viewUnit()) {
        weight = { weight: text };
      } else {
        try {
          weight = { weight_grams: await parseWeightOnServer(text, chosen, currentTimeZone()) };
        } catch (err) {
          setLogError(toApiError(err).message);
          return;
        }
      }
      const result = await runBody({ type: "log_entry", ...weight, ...(noteText ? { note: noteText } : {}) });
      if (result?.ok) {
        setWeightText("");
        setNote("");
        writeString("local", WEIGHT_UNIT_KEY, chosen);
      } else if (result?.error) {
        setLogError(result.error);
      }
    } finally {
      setLogPending(false);
    }
  }

  async function saveSettings(e: SubmitEvent) {
    e.preventDefault();
    if (settingsPending()) return;
    const goal = goalText().trim();
    setSettingsError(null);
    setSettingsPending(true);
    try {
      // No goal clears it; the goal text is parsed by the engine in the new unit.
      const result = await runBody({ type: "update_settings", unit: settingsUnit(), ...(goal ? { goal } : {}) });
      if (!result?.ok && result?.error) setSettingsError(result.error);
    } finally {
      setSettingsPending(false);
    }
  }

  return (
    <div class="weight">
      <div class="weight-inner">
        <Show when={body.actionError}>
          {(message) => (
            <div class="notice" role="alert">
              <span>{message()}</span>
              <button class="banner-close" type="button" onClick={dismissBodyError} aria-label={t("common.dismiss")}>
                ×
              </button>
            </div>
          )}
        </Show>

        <Show
          when={body.loaded}
          fallback={
            <Show
              when={body.loadError}
              fallback={
                <p class="muted center" role="status">
                  {t("weight.loading")}
                </p>
              }
            >
              {(message) => (
                <div class="empty" role="alert">
                  <p class="empty-title">{message()}</p>
                  <button class="btn" type="button" onClick={() => void refreshBody()}>
                    {t("common.tryAgain")}
                  </button>
                </div>
              )}
            </Show>
          }
        >
          <Show when={body.loadError}>
            {(message) => (
              <p class="muted small" role="status">
                {t("weight.staleCopy", { message: message() })}
              </p>
            )}
          </Show>

          <form class="card weight-log" onSubmit={(e) => void submitLog(e)}>
            <h1 class="view-title">{t("weight.title")}</h1>
            <div class="weight-log-row">
              <label class="sr-only" for="weight-input">
                {t("weight.weight")}
              </label>
              <input
                id="weight-input"
                class="text-input weight-input"
                type="text"
                inputmode="decimal"
                placeholder={t("weight.weightIn", { unit: unit() })}
                value={weightText()}
                onInput={(e) => setWeightText(e.currentTarget.value)}
                autocomplete="off"
                disabled={logPending()}
              />
              <div class="segmented" role="group" aria-label={t("weight.unitGroup")}>
                <button
                  class="segmented-btn"
                  classList={{ "segmented-btn--active": unit() === "kg" }}
                  type="button"
                  onClick={() => chooseUnit("kg")}
                  aria-pressed={unit() === "kg" ? "true" : "false"}
                >
                  kg
                </button>
                <button
                  class="segmented-btn"
                  classList={{ "segmented-btn--active": unit() === "lb" }}
                  type="button"
                  onClick={() => chooseUnit("lb")}
                  aria-pressed={unit() === "lb" ? "true" : "false"}
                >
                  lb
                </button>
              </div>
            </div>
            <label class="sr-only" for="weight-note">
              {t("weight.noteOptional")}
            </label>
            <input
              id="weight-note"
              class="text-input"
              type="text"
              placeholder={t("weight.noteOptional")}
              value={note()}
              onInput={(e) => setNote(e.currentTarget.value)}
              maxlength="1000"
              autocomplete="off"
              disabled={logPending()}
            />
            <Show when={logError()}>{(message) => <p class="form-error">{message()}</p>}</Show>
            <button class="btn btn--primary btn--block" type="submit" disabled={logPending()}>
              {logPending() ? t("weight.logging") : t("weight.log")}
            </button>
          </form>

          <Show when={body.view}>
            {(view) => (
              <>
                <div class="stat-grid">
                  <div class="stat-card" aria-label={t("weight.current")}>
                    <span class="stat-value">{statLabel(view(), "current")}</span>
                    <span class="stat-label">{t("weight.current")}</span>
                  </div>
                  <div class="stat-card" aria-label={t("weight.avg7")}>
                    <span class="stat-value">{statLabel(view(), "avg7")}</span>
                    <span class="stat-label">{t("weight.avg7")}</span>
                  </div>
                  <div class="stat-card" aria-label={t("weight.weeklyChange")}>
                    <span class="stat-value">{statLabel(view(), "weekly_change")}</span>
                    <span class="stat-label">{t("weight.weeklyChange")}</span>
                  </div>
                  <div class="stat-card" aria-label={t("weight.goalDistance")}>
                    <Show when={goalValue()} fallback={<span class="stat-value stat-value--muted">{t("weight.setGoal")}</span>}>
                      {(value) => <span class="stat-value">{value()}</span>}
                    </Show>
                    <span class="stat-label">
                      {t("weight.goalDistance")}
                      <Show when={view().stat_labels.goal}>{(goal) => ` · ${goal()}`}</Show>
                    </span>
                  </div>
                </div>
                <Show when={detailLine()}>
                  <p class="stat-detail muted small">{detailLine()}</p>
                </Show>

                <section class="card weight-chart-card">
                  <h2 class="section-title">{t("weight.trend")}</h2>
                  <LineChart
                    series={chartData().series}
                    xLabels={chartData().labels}
                    ariaLabel={t("weight.chartLabel")}
                    height={180}
                    formatY={(value) => String(Math.round(value * 10) / 10)}
                  />
                </section>

                <Show when={props.compact}>
                  <A href="/weight" class="weight-all-link">
                    {t("weight.allEntries")}
                  </A>
                </Show>
                <Show when={!props.compact}>
                <section class="weight-history">
                  <h2 class="section-title">{t("weight.history")}</h2>
                  <Show
                    when={entries().length > 0}
                    fallback={
                      <div class="empty">
                        <p class="empty-title">{t("weight.empty")}</p>
                      </div>
                    }
                  >
                    <ul class="weight-list">
                      <For each={entries()}>
                        {(entry) => <EntryRow entry={entry} unit={viewUnit()} label={view().labels[entry.id]} />}
                      </For>
                    </ul>
                  </Show>
                </section>

                <details class="settings-block">
                  <summary>{t("weight.settings")}</summary>
                  <form class="settings-form" onSubmit={(e) => void saveSettings(e)}>
                    <label class="field-label" for="settings-unit">
                      {t("weight.unit")}
                    </label>
                    <select
                      id="settings-unit"
                      class="text-input"
                      value={settingsUnit()}
                      onChange={(e) => {
                        const next: WeightUnit = e.currentTarget.value === "lb" ? "lb" : "kg";
                        // The engine reads the goal in the new unit, so re-express an unchanged goal.
                        const goal = view().settings.goal_grams;
                        if (goal !== null && goalText().trim() === formatWeight(goal, settingsUnit())) {
                          setGoalText(formatWeight(goal, next));
                        }
                        setSettingsUnit(next);
                      }}
                      disabled={settingsPending()}
                    >
                      <option value="kg">{t("weight.kilograms")}</option>
                      <option value="lb">{t("weight.pounds")}</option>
                    </select>
                    <label class="field-label" for="settings-goal">
                      {t("weight.goalWeight", { unit: settingsUnit() })}
                    </label>
                    <input
                      id="settings-goal"
                      class="text-input"
                      type="text"
                      inputmode="decimal"
                      placeholder={t("weight.noGoal")}
                      value={goalText()}
                      onInput={(e) => setGoalText(e.currentTarget.value)}
                      autocomplete="off"
                      disabled={settingsPending()}
                    />
                    <Show when={settingsError()}>{(message) => <p class="form-error">{message()}</p>}</Show>
                    <button class="btn btn--primary" type="submit" disabled={settingsPending()}>
                      {t("weight.saveSettings")}
                    </button>
                  </form>
                </details>
                </Show>
              </>
            )}
          </Show>
        </Show>
      </div>
    </div>
  );
}
