import { A } from "@solidjs/router";
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  onMount,
  Show,
} from "solid-js";
import {
  parseWeightOnServer,
  type BodyEntry,
  type BodyStatLabel,
  type BodyView,
} from "../api/body";
import { toApiError } from "../api/client";
import { type ChartSeries, LineChart } from "../components/LineChart";
import {
  Page,
  PageHeader,
  PageGrid,
  Card,
  RowList,
  ListRow,
  Button,
  EmptyState,
  LoadingState,
  ErrorState,
  Sheet,
  useWide,
} from "../components/page";
import { t } from "../i18n";
import { readString, writeString } from "../lib/storage";
import {
  formatDate,
  formatDateTime,
  formatWeight,
  weightValue,
  type WeightUnit,
} from "../lib/units";
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

function EntryRow(props: {
  entry: BodyEntry;
  unit: WeightUnit;
  label: string | undefined;
  wide?: boolean;
}) {
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
    const changes: { weight?: string; measured_ms?: number; note?: string } =
      {};
    if (weight().trim() !== formatWeight(props.entry.weight_grams, props.unit))
      changes.weight = weight().trim();
    if (when() !== localDateTime(props.entry.measured_ms))
      changes.measured_ms = measured;
    if (note().trim() !== props.entry.note) changes.note = note().trim();
    if (Object.keys(changes).length === 0) {
      setEditing(false);
      return;
    }
    setError(null);
    setPending(true);
    try {
      const result = await runBody({
        type: "edit_entry",
        entry_id: props.entry.id,
        ...changes,
      });
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

  const editForm = () => (
    <form
      class={props.wide ? "pg-field" : "weight-edit"}
      onSubmit={(e) => void save(e)}
      aria-label={t("weight.editForm")}
    >
      <label
        class={props.wide ? "pg-field-label" : "field-label"}
        for={`edit-weight-${props.entry.id}`}
      >
        {t("weight.weightIn", { unit: props.unit })}
      </label>
      <input
        id={`edit-weight-${props.entry.id}`}
        class={props.wide ? "pg-input" : "text-input"}
        type="text"
        inputmode="decimal"
        value={weight()}
        onInput={(e) => setWeight(e.currentTarget.value)}
        autocomplete="off"
        disabled={pending()}
      />
      <label
        class={props.wide ? "pg-field-label" : "field-label"}
        for={`edit-when-${props.entry.id}`}
      >
        {t("weight.when")}
      </label>
      <input
        id={`edit-when-${props.entry.id}`}
        class={props.wide ? "pg-input" : "text-input"}
        type="datetime-local"
        value={when()}
        onInput={(e) => setWhen(e.currentTarget.value)}
        disabled={pending()}
      />
      <label
        class={props.wide ? "pg-field-label" : "field-label"}
        for={`edit-note-${props.entry.id}`}
      >
        {t("weight.note")}
      </label>
      <input
        id={`edit-note-${props.entry.id}`}
        class={props.wide ? "pg-input" : "text-input"}
        type="text"
        value={note()}
        onInput={(e) => setNote(e.currentTarget.value)}
        maxlength="1000"
        autocomplete="off"
        disabled={pending()}
      />
      <Show when={error()}>
        {(message) => (
          <p class={props.wide ? "pg-notice" : "form-error"}>{message()}</p>
        )}
      </Show>
      <div class={props.wide ? "pg-card-actions" : "weight-edit-actions"}>
        <button
          class={props.wide ? "pg-btn" : "btn btn--small"}
          type="button"
          onClick={() => setEditing(false)}
          disabled={pending()}
        >
          {t("common.cancel")}
        </button>
        <button
          class={props.wide ? "pg-btn" : "btn btn--primary btn--small"}
          type="submit"
          disabled={pending()}
        >
          {t("common.save")}
        </button>
      </div>
    </form>
  );
  return (
    <Show when={props.wide} fallback={phone()}>
      <ListRow
        title={props.label ?? DASH}
        sub={props.entry.note}
        meta={formatDateTime(props.entry.measured_ms)}
        trailing={
          <>
            <Button variant="ghost" disabled={pending()} onClick={startEdit}>
              {t("common.edit")}
            </Button>
            <Show
              when={confirmDelete()}
              fallback={
                <Button
                  variant="ghost"
                  disabled={pending()}
                  onClick={() => setConfirmDelete(true)}
                >
                  {t("common.delete")}
                </Button>
              }
            >
              <Button
                variant="danger"
                disabled={pending()}
                onClick={() => void remove()}
              >
                {t("common.confirmDelete")}
              </Button>
              <Button
                variant="ghost"
                disabled={pending()}
                onClick={() => setConfirmDelete(false)}
              >
                {t("common.cancel")}
              </Button>
            </Show>
            <Sheet
              open={editing()}
              onClose={() => setEditing(false)}
              title={t("weight.editForm")}
            >
              {editForm()}
            </Sheet>
          </>
        }
      />
    </Show>
  );
  function phone() {
    return (
      <li class="weight-row" classList={{ "weight-row--pending": pending() }}>
        <Show when={!editing()} fallback={editForm()}>
          <div class="weight-row-main">
            <div class="weight-row-text">
              <span class="weight-row-weight">{props.label ?? DASH}</span>
              <span class="muted small">
                {formatDateTime(props.entry.measured_ms)}
              </span>
              <Show when={props.entry.note}>
                {(text) => <span class="weight-row-note">{text()}</span>}
              </Show>
            </div>
            <div class="weight-row-actions">
              <button
                class="btn btn--small"
                type="button"
                onClick={startEdit}
                disabled={pending()}
              >
                {t("common.edit")}
              </button>
              <Show
                when={confirmDelete()}
                fallback={
                  <button
                    class="btn btn--small"
                    type="button"
                    onClick={() => setConfirmDelete(true)}
                    disabled={pending()}
                  >
                    {t("common.delete")}
                  </button>
                }
              >
                <button
                  class="btn btn--small btn--danger"
                  type="button"
                  onClick={() => void remove()}
                  disabled={pending()}
                >
                  {t("common.confirmDelete")}
                </button>
                <button
                  class="btn btn--small"
                  type="button"
                  onClick={() => setConfirmDelete(false)}
                  disabled={pending()}
                >
                  {t("common.cancel")}
                </button>
              </Show>
            </div>
          </div>
        </Show>
      </li>
    );
  }
}

/** `compact`: the Training page's summary (log, stats, chart); the entries and settings stay on /weight. */
export default function WeightView(props: { compact?: boolean } = {}) {
  const { wide, track } = useWide();
  const desktop = () => wide() && !props.compact;
  const stored = readString("local", WEIGHT_UNIT_KEY);
  const storedUnit: WeightUnit =
    stored === "kg" || stored === "lb" ? stored : "kg";
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
    setGoalText(
      view.settings.goal_grams === null
        ? ""
        : formatWeight(view.settings.goal_grams, view.settings.unit),
    );
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

  const chartData = createMemo<{ series: ChartSeries[]; labels: string[] }>(
    () => {
      const view = body.view;
      const points = view?.series ?? [];
      const u = viewUnit();
      const series: ChartSeries[] = [
        {
          key: "weight",
          points: points.map((p) => ({
            x: p.measured_ms,
            y: weightValue(p.weight_grams, u),
          })),
          class: "chart-line--primary",
        },
      ];
      const first = points[0];
      const last = points[points.length - 1];
      const labels =
        first && last && points.length > 1
          ? [formatDate(first.measured_ms), formatDate(last.measured_ms)]
          : [];
      return { series, labels };
    },
  );

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
          weight = {
            weight_grams: await parseWeightOnServer(
              text,
              chosen,
              currentTimeZone(),
            ),
          };
        } catch (err) {
          setLogError(toApiError(err).message);
          return;
        }
      }
      const result = await runBody({
        type: "log_entry",
        ...weight,
        ...(noteText ? { note: noteText } : {}),
      });
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
      const result = await runBody({
        type: "update_settings",
        unit: settingsUnit(),
        ...(goal ? { goal } : {}),
      });
      if (!result?.ok && result?.error) setSettingsError(result.error);
    } finally {
      setSettingsPending(false);
    }
  }

  const logForm = () => (
    <form
      class={desktop() ? "pg-field" : "card weight-log"}
      onSubmit={(e) => void submitLog(e)}
    >
      <Show when={!desktop()}>
        <h1 class="view-title">{t("weight.title")}</h1>
      </Show>
      <div class={desktop() ? "weight-desk-log-row" : "weight-log-row"}>
        <label class="sr-only" for="weight-input">
          {t("weight.weight")}
        </label>
        <input
          id="weight-input"
          class={desktop() ? "pg-input" : "text-input weight-input"}
          type="text"
          inputmode="decimal"
          placeholder={t("weight.weightIn", { unit: unit() })}
          value={weightText()}
          onInput={(e) => setWeightText(e.currentTarget.value)}
          autocomplete="off"
          disabled={logPending()}
        />
        <div
          class={desktop() ? "weight-desk-units" : "segmented"}
          role="group"
          aria-label={t("weight.unitGroup")}
        >
          <button
            class={desktop() ? "pg-btn" : "segmented-btn"}
            classList={{
              "segmented-btn--active": !desktop() && unit() === "kg",
            }}
            type="button"
            onClick={() => chooseUnit("kg")}
            aria-pressed={unit() === "kg" ? "true" : "false"}
          >
            kg
          </button>
          <button
            class={desktop() ? "pg-btn" : "segmented-btn"}
            classList={{
              "segmented-btn--active": !desktop() && unit() === "lb",
            }}
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
        class={desktop() ? "pg-input" : "text-input"}
        type="text"
        placeholder={t("weight.noteOptional")}
        value={note()}
        onInput={(e) => setNote(e.currentTarget.value)}
        maxlength="1000"
        autocomplete="off"
        disabled={logPending()}
      />
      <Show when={logError()}>
        {(message) => (
          <p class={desktop() ? "pg-notice" : "form-error"}>{message()}</p>
        )}
      </Show>
      <button
        class={desktop() ? "pg-btn" : "btn btn--primary btn--block"}
        type="submit"
        disabled={logPending()}
      >
        {logPending() ? t("weight.logging") : t("weight.log")}
      </button>
    </form>
  );
  const settingsForm = () => (
    <form
      class={desktop() ? "pg-field" : "settings-form"}
      onSubmit={(e) => void saveSettings(e)}
    >
      <label
        class={desktop() ? "pg-field-label" : "field-label"}
        for="settings-unit"
      >
        {t("weight.unit")}
      </label>
      <select
        id="settings-unit"
        class={desktop() ? "pg-input" : "text-input"}
        value={settingsUnit()}
        onChange={(e) => {
          const next: WeightUnit = e.currentTarget.value === "lb" ? "lb" : "kg";
          // The engine reads the goal in the new unit, so re-express an unchanged goal.
          const goal = body.view!.settings.goal_grams;
          if (
            goal !== null &&
            goalText().trim() === formatWeight(goal, settingsUnit())
          ) {
            setGoalText(formatWeight(goal, next));
          }
          setSettingsUnit(next);
        }}
        disabled={settingsPending()}
      >
        <option value="kg">{t("weight.kilograms")}</option>
        <option value="lb">{t("weight.pounds")}</option>
      </select>
      <label
        class={desktop() ? "pg-field-label" : "field-label"}
        for="settings-goal"
      >
        {t("weight.goalWeight", { unit: settingsUnit() })}
      </label>
      <input
        id="settings-goal"
        class={desktop() ? "pg-input" : "text-input"}
        type="text"
        inputmode="decimal"
        placeholder={t("weight.noGoal")}
        value={goalText()}
        onInput={(e) => setGoalText(e.currentTarget.value)}
        autocomplete="off"
        disabled={settingsPending()}
      />
      <Show when={settingsError()}>
        {(message) => (
          <p class={desktop() ? "pg-notice" : "form-error"}>{message()}</p>
        )}
      </Show>
      <button
        class={desktop() ? "pg-btn" : "btn btn--primary"}
        type="submit"
        disabled={settingsPending()}
      >
        {t("weight.saveSettings")}
      </button>
    </form>
  );
  return (
    <div class="weight" ref={track}>
      <Show when={desktop()} fallback={phone()}>
        <Page class="weight-desk">
          <PageHeader
            title={t("weight.title")}
            context={
              body.view?.stats.current_measured_ms != null
                ? `${statLabel(body.view, "current")} · ${formatDateTime(body.view!.stats.current_measured_ms!)}`
                : undefined
            }
            action={
              <a class="pg-link" href="/training">
                {t("weight.backTraining")}
              </a>
            }
          />
          <Show when={body.actionError}>
            {(m) => (
              <div class="pg-notice" role="alert">
                {m()}
                <Button
                  variant="ghost"
                  aria-label={t("common.dismiss")}
                  onClick={dismissBodyError}
                >
                  ×
                </Button>
              </div>
            )}
          </Show>
          <Show
            when={body.loaded ? body.view : null}
            fallback={
              <Show
                when={body.loadError}
                fallback={<LoadingState label={t("weight.loading")} />}
              >
                {(m) => (
                  <ErrorState
                    message={m()}
                    retryLabel={t("common.tryAgain")}
                    onRetry={() => void refreshBody()}
                  />
                )}
              </Show>
            }
          >
            {(view) => (
              <>
                <Show when={body.loadError}>
                  {(m) => (
                    <p class="pg-notice" role="status">
                      {t("weight.staleCopy", { message: m() })}
                    </p>
                  )}
                </Show>
                <PageGrid
                  layout="rail"
                  rail={
                    <>
                      <Card title={t("weight.log")}>{logForm()}</Card>
                      <Card title={t("weight.stats")}>
                        <dl class="pg-stats">
                          <div class="pg-stat">
                            <dt>{t("weight.current")}</dt>
                            <dd>{statLabel(view(), "current")}</dd>
                          </div>
                          <div class="pg-stat">
                            <dt>{t("weight.avg7")}</dt>
                            <dd>{statLabel(view(), "avg7")}</dd>
                          </div>
                          <div class="pg-stat">
                            <dt>{t("weight.weeklyChange")}</dt>
                            <dd>{statLabel(view(), "weekly_change")}</dd>
                          </div>
                          <div class="pg-stat">
                            <dt>{t("weight.goalDistance")}</dt>
                            <dd>
                              {goalValue() ?? t("weight.setGoal")}
                              <Show when={view().stat_labels.goal}>
                                {(g) => ` · ${g()}`}
                              </Show>
                            </dd>
                          </div>
                        </dl>
                        <Show when={detailLine()}>
                          <p class="pg-muted">{detailLine()}</p>
                        </Show>
                      </Card>
                      <Card title={t("weight.settings")}>{settingsForm()}</Card>
                    </>
                  }
                >
                  <Card title={t("weight.trend")}>
                    <Show
                      when={view().series.length > 1}
                      fallback={<EmptyState text={t("weight.chartEmpty")} />}
                    >
                      <LineChart
                        series={chartData().series}
                        xLabels={chartData().labels}
                        ariaLabel={t("weight.chartLabel")}
                        height={180}
                        formatY={(value) => String(Math.round(value * 10) / 10)}
                      />
                    </Show>
                  </Card>
                  <Card list title={t("weight.history")}>
                    <Show
                      when={entries().length}
                      fallback={<EmptyState text={t("weight.emptyRail")} />}
                    >
                      <RowList label={t("weight.history")}>
                        <For each={entries()}>
                          {(entry) => (
                            <EntryRow
                              wide
                              entry={entry}
                              unit={viewUnit()}
                              label={view().labels[entry.id]}
                            />
                          )}
                        </For>
                      </RowList>
                    </Show>
                  </Card>
                </PageGrid>
              </>
            )}
          </Show>
        </Page>
      </Show>
    </div>
  );
  function phone() {
    return (
      <>
        <div class="weight-inner">
          <Show when={body.actionError}>
            {(message) => (
              <div class="notice" role="alert">
                <span>{message()}</span>
                <button
                  class="banner-close"
                  type="button"
                  onClick={dismissBodyError}
                  aria-label={t("common.dismiss")}
                >
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
                    <button
                      class="btn"
                      type="button"
                      onClick={() => void refreshBody()}
                    >
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

            {logForm()}

            <Show when={body.view}>
              {(view) => (
                <>
                  <div class="stat-grid">
                    <div class="stat-card" aria-label={t("weight.current")}>
                      <span class="stat-value">
                        {statLabel(view(), "current")}
                      </span>
                      <span class="stat-label">{t("weight.current")}</span>
                    </div>
                    <div class="stat-card" aria-label={t("weight.avg7")}>
                      <span class="stat-value">
                        {statLabel(view(), "avg7")}
                      </span>
                      <span class="stat-label">{t("weight.avg7")}</span>
                    </div>
                    <div
                      class="stat-card"
                      aria-label={t("weight.weeklyChange")}
                    >
                      <span class="stat-value">
                        {statLabel(view(), "weekly_change")}
                      </span>
                      <span class="stat-label">{t("weight.weeklyChange")}</span>
                    </div>
                    <div
                      class="stat-card"
                      aria-label={t("weight.goalDistance")}
                    >
                      <Show
                        when={goalValue()}
                        fallback={
                          <span class="stat-value stat-value--muted">
                            {t("weight.setGoal")}
                          </span>
                        }
                      >
                        {(value) => <span class="stat-value">{value()}</span>}
                      </Show>
                      <span class="stat-label">
                        {t("weight.goalDistance")}
                        <Show when={view().stat_labels.goal}>
                          {(goal) => ` · ${goal()}`}
                        </Show>
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
                            {(entry) => (
                              <EntryRow
                                entry={entry}
                                unit={viewUnit()}
                                label={view().labels[entry.id]}
                              />
                            )}
                          </For>
                        </ul>
                      </Show>
                    </section>

                    <details class="settings-block">
                      <summary>{t("weight.settings")}</summary>
                      {settingsForm()}
                    </details>
                  </Show>
                </>
              )}
            </Show>
          </Show>
        </div>
      </>
    );
  }
}
