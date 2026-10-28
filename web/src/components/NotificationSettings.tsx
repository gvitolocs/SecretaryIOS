import { createSignal, For, onMount, Show } from "solid-js";
import { CATEGORIES, type Category, type NotificationSettings as Settings } from "../api/notifications";
import { locale, t } from "../i18n";
import {
  CATEGORY_DESC,
  CATEGORY_TITLE,
  loadSettings,
  pushBusy,
  pushStatus,
  refreshPushState,
  removeDevice,
  sendTest,
  settingsView,
  thisSubscriptionId,
  turnOffPush,
  turnOnPush,
  updateSettings,
} from "../state/notifications";
import { relativeTime } from "./NotificationBell";

/** Approval requests are always on (the owner would otherwise miss a request), so they have no switch. */
const TOGGLED = CATEGORIES.filter((c) => c !== "approval");

export const LEAD_CHOICES = [5, 10, 15, 30, 60] as const;

/** Epoch ms of tomorrow 08:00 local time. */
export function tomorrowMorning(now = new Date()): number {
  const d = new Date(now);
  d.setDate(d.getDate() + 1);
  d.setHours(8, 0, 0, 0);
  return d.getTime();
}

function Toggle(props: { id: string; label: string; hint?: string; checked: boolean; disabled?: boolean; onChange: (v: boolean) => void }) {
  return (
    <label class="switch-row" for={props.id}>
      <span class="switch-text">
        <span class="switch-label">{props.label}</span>
        <Show when={props.hint}>
          <span class="muted small">{props.hint}</span>
        </Show>
      </span>
      <input
        id={props.id}
        class="switch"
        type="checkbox"
        role="switch"
        checked={props.checked}
        disabled={props.disabled}
        onChange={(e) => props.onChange(e.currentTarget.checked)}
      />
    </label>
  );
}

/** Notification settings, in the account panel. */
export function NotificationSettings() {
  const [error, setError] = createSignal<string | null>(null);
  const [status, setStatus] = createSignal<{ ok: boolean; message: string } | null>(null);
  const [testing, setTesting] = createSignal(false);

  onMount(() => {
    void loadSettings();
    void refreshPushState();
  });

  const s = () => settingsView()?.settings;

  async function change(f: (s: Settings) => void) {
    setError(null);
    const err = await updateSettings((cur) => {
      f(cur);
      return cur;
    });
    if (err) setError(t("notify.saveFailed", { error: err }));
  }

  function setCategory(c: Category, on: boolean) {
    void change((cur) => {
      cur.categories[c] = on;
    });
  }

  const dndActive = () => {
    const until = s()?.dndUntil;
    return typeof until === "number" && until > Date.now() ? until : null;
  };

  const timeFmt = (ms: number) => {
    try {
      return new Intl.DateTimeFormat(locale(), { weekday: "short", hour: "2-digit", minute: "2-digit" }).format(ms);
    } catch {
      return new Date(ms).toLocaleString();
    }
  };

  return (
    <div class="settings-form notif-settings" aria-labelledby="notif-settings-title">
      <h3 id="notif-settings-title" class="field-label notif-settings-title">
        {t("notify.settings")}
      </h3>

      {/* This device's push. */}
      <div class="notif-block">
        <span class="field-label appearance-label">{t("notify.pushTitle")}</span>
        <p class="muted small" role="status">
          {pushStatus() === "on"
            ? t("notify.pushOn")
            : pushStatus() === "denied"
              ? t("notify.pushDenied")
              : pushStatus() === "needs-install"
                ? t("notify.pushInstallIos")
                : pushStatus() === "unsupported"
                  ? t("notify.pushUnsupported")
                  : t("notify.pushOff")}
        </p>
        <div class="notif-actions">
          <Show when={pushStatus() === "off"}>
            <button
              type="button"
              class="btn btn--primary btn--small"
              disabled={pushBusy()}
              onClick={() => {
                setError(null);
                void turnOnPush().then((e) => e && setError(e));
              }}
            >
              {pushBusy() ? t("notify.pushWorking") : t("notify.pushEnable")}
            </button>
          </Show>
          <Show when={pushStatus() === "on"}>
            <button
              type="button"
              class="btn btn--small"
              disabled={testing()}
              onClick={async () => {
                setTesting(true);
                setStatus(await sendTest());
                setTesting(false);
              }}
            >
              {testing() ? t("notify.pushWorking") : t("notify.pushTest")}
            </button>
            <button type="button" class="btn btn--small" disabled={pushBusy()} onClick={() => void turnOffPush()}>
              {t("notify.pushDisable")}
            </button>
          </Show>
        </div>
        <Show when={status()}>
          {(st) => (
            <p class={st().ok ? "muted small" : "form-error small"} role={st().ok ? "status" : "alert"}>
              {st().message}
            </p>
          )}
        </Show>
      </div>

      <Show when={s()}>
        {(cur) => (
          <>
            <div class="notif-block">
              <span class="field-label appearance-label">{t("notify.categoriesTitle")}</span>
              <For each={TOGGLED}>
                {(c) => (
                  <Toggle
                    id={`notif-cat-${c}`}
                    label={t(CATEGORY_TITLE[c])}
                    hint={t(CATEGORY_DESC[c])}
                    checked={cur().categories[c] === true}
                    onChange={(v) => setCategory(c, v)}
                  />
                )}
              </For>
              <label class="field-label notif-inline" for="notif-lead">
                {t("notify.calendarLead")}
                <select
                  id="notif-lead"
                  class="text-input notif-select"
                  value={String(cur().calendarLeadMinutes)}
                  disabled={!cur().categories.calendar}
                  onChange={(e) => {
                    const v = Number(e.currentTarget.value);
                    void change((x) => {
                      x.calendarLeadMinutes = v;
                    });
                  }}
                >
                  <For each={LEAD_CHOICES}>{(m) => <option value={m}>{t("notify.minutes", { count: m })}</option>}</For>
                </select>
              </label>
            </div>

            <div class="notif-block">
              <span class="field-label appearance-label" id="notif-dnd">
                {t("notify.dndTitle")}
              </span>
              <div class="segmented segmented--wide" role="group" aria-labelledby="notif-dnd">
                <button
                  type="button"
                  class="segmented-btn"
                  classList={{ "segmented-btn--active": !dndActive() }}
                  aria-pressed={!dndActive() ? "true" : "false"}
                  onClick={() => void change((x) => (x.dndUntil = null))}
                >
                  {t("notify.dndOff")}
                </button>
                <button
                  type="button"
                  class="segmented-btn"
                  onClick={() => void change((x) => (x.dndUntil = Date.now() + 3_600_000))}
                >
                  {t("notify.dnd1h")}
                </button>
                <button type="button" class="segmented-btn" onClick={() => void change((x) => (x.dndUntil = tomorrowMorning()))}>
                  {t("notify.dndMorning")}
                </button>
              </div>
              <Show when={dndActive()}>
                {(until) => <p class="muted small">{t("notify.dndUntil", { time: timeFmt(until()) })}</p>}
              </Show>
            </div>

            <div class="notif-block">
              <span class="field-label appearance-label">{t("notify.quietTitle")}</span>
              <Toggle
                id="notif-quiet"
                label={t("notify.quietEnable")}
                checked={cur().quietHours.enabled}
                onChange={(v) => void change((x) => (x.quietHours.enabled = v))}
              />
              <div class="notif-times">
                <label class="field-label" for="notif-quiet-from">
                  {t("notify.quietFrom")}
                  <input
                    id="notif-quiet-from"
                    class="text-input"
                    type="time"
                    value={cur().quietHours.start}
                    disabled={!cur().quietHours.enabled}
                    onChange={(e) => {
                      const v = e.currentTarget.value;
                      if (/^\d\d:\d\d$/.test(v)) void change((x) => (x.quietHours.start = v));
                    }}
                  />
                </label>
                <label class="field-label" for="notif-quiet-to">
                  {t("notify.quietTo")}
                  <input
                    id="notif-quiet-to"
                    class="text-input"
                    type="time"
                    value={cur().quietHours.end}
                    disabled={!cur().quietHours.enabled}
                    onChange={(e) => {
                      const v = e.currentTarget.value;
                      if (/^\d\d:\d\d$/.test(v)) void change((x) => (x.quietHours.end = v));
                    }}
                  />
                </label>
              </div>
              <Toggle
                id="notif-bypass"
                label={t("notify.remindersBypass")}
                checked={cur().remindersBypassQuiet}
                onChange={(v) => void change((x) => (x.remindersBypassQuiet = v))}
              />
              <Toggle
                id="notif-digest"
                label={t("notify.digest")}
                checked={cur().digest}
                onChange={(v) => void change((x) => (x.digest = v))}
              />
              <Toggle
                id="notif-previews"
                label={t("notify.previews")}
                checked={cur().previews}
                onChange={(v) => void change((x) => (x.previews = v))}
              />
            </div>

            <div class="notif-block">
              <span class="field-label appearance-label">{t("notify.devicesTitle")}</span>
              <Show
                when={(settingsView()?.subscriptions.length ?? 0) > 0}
                fallback={<p class="muted small">{t("notify.devicesEmpty")}</p>}
              >
                <ul class="notif-devices">
                  <For each={settingsView()?.subscriptions ?? []}>
                    {(d) => {
                      const prefs = () => cur().devices[d.id];
                      const enabled = () => prefs()?.enabled !== false;
                      return (
                        <li class="notif-device">
                          <div class="notif-device-text">
                            <span class="notif-device-name">
                              {d.label}
                              <Show when={d.id === thisSubscriptionId()}>
                                <span class="notif-chip">{t("notify.deviceThis")}</span>
                              </Show>
                            </span>
                            <span class="muted small">
                              {d.failures > 0
                                ? t("notify.deviceFailing")
                                : d.lastSuccessAt
                                  ? t("notify.deviceLast", { time: relativeTime(d.lastSuccessAt) })
                                  : t("notify.deviceNever")}
                            </span>
                          </div>
                          <input
                            class="switch"
                            type="checkbox"
                            role="switch"
                            aria-label={`${t("notify.deviceMute")}: ${d.label}`}
                            checked={enabled()}
                            onChange={(e) => {
                              const v = e.currentTarget.checked;
                              void change((x) => {
                                x.devices[d.id] = { enabled: v, categories: x.devices[d.id]?.categories ?? {} };
                              });
                            }}
                          />
                          <button
                            type="button"
                            class="btn-link notif-remove"
                            aria-label={`${t("notify.deviceRemove")}: ${d.label}`}
                            onClick={() => void removeDevice(d.id).then((e) => e && setError(t("notify.saveFailed", { error: e })))}
                          >
                            {t("notify.deviceRemove")}
                          </button>
                        </li>
                      );
                    }}
                  </For>
                </ul>
              </Show>
            </div>
          </>
        )}
      </Show>
      <Show when={error()}>
        {(msg) => (
          <p class="form-error" role="alert">
            {msg()}
          </p>
        )}
      </Show>
    </div>
  );
}
