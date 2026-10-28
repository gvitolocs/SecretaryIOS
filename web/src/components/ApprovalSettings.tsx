// "Approvals" in the account panel: which device gets owner-approval requests first. With none picked, every
// device of yours sees a request at once. It also says which devices have Face ID set up (needed for sensitive
// requests). Shown only when the server has the approvals routes.

import { createSignal, For, onMount, Show } from "solid-js";
import { approvalsApi, type ApprovalSettings as Settings } from "../api/approvals";
import { Card } from "./page";
import { t } from "../i18n";

export function ApprovalSettings(props: { desktop?: boolean }) {
  const [settings, setSettings] = createSignal<Settings | null>(null);
  const [saving, setSaving] = createSignal(false);
  const [saved, setSaved] = createSignal(false);
  const [error, setError] = createSignal<"save" | null>(null);

  onMount(async () => {
    try {
      setSettings(await approvalsApi.settings());
    } catch {
      // 404: the server doesn't have approvals yet; any other failure: the section just isn't offered. Say nothing.
    }
  });

  async function pick(value: string) {
    const cur = settings();
    if (!cur || saving()) return;
    const id = value === "" ? null : value;
    setSaving(true);
    setSaved(false);
    setError(null);
    try {
      await approvalsApi.setMainDevice(id);
      setSettings({ ...cur, mainDeviceId: id, devices: cur.devices.map((d) => ({ ...d, isMain: d.id === id })) });
      setSaved(true);
    } catch {
      // Put the select back to what the server has.
      setSettings({ ...cur });
      setError("save");
    } finally {
      setSaving(false);
    }
  }

  const withFaceId = () => settings()?.devices.filter((d) => d.hasBiometricKey) ?? [];

  const form = () => (
      <div class={props.desktop ? "pg-form" : "settings-form"} aria-labelledby={props.desktop ? undefined : "apv-settings-title"}>
        <Show when={!props.desktop}><h3 class={props.desktop ? "pg-field-label" : "field-label"} id="apv-settings-title">
          {t("approvals.settingsTitle")}
        </h3></Show>
        <Show when={settings()}>
          {(cur) => (
            <>
              <label class={props.desktop ? "pg-field-label" : "field-label"} for="apv-main-device">
                {t("approvals.mainLabel")}
              </label>
              <select
                id="apv-main-device"
                class={props.desktop ? "pg-input" : "text-input"}
                value={cur().mainDeviceId ?? ""}
                disabled={saving()}
                aria-describedby="apv-main-hint"
                onChange={(e) => void pick(e.currentTarget.value)}
              >
                <option value="" selected={cur().mainDeviceId === null}>
                  {t("approvals.mainNone")}
                </option>
                <For each={cur().devices}>
                  {(d) => (
                    <option value={d.id} selected={cur().mainDeviceId === d.id}>
                      {d.id === cur().thisDeviceId ? t("approvals.mainThis", { name: d.name }) : d.name}
                    </option>
                  )}
                </For>
              </select>
              <p id="apv-main-hint" class={props.desktop ? "pg-muted" : "muted small"}>
                {t("approvals.mainHint", { secs: cur().escalateAfterSecs })}
              </p>
              <p class={props.desktop ? "pg-muted" : "muted small"} data-testid="apv-faceid-devices">
                {withFaceId().length
                  ? t("approvals.faceIdOn", { names: withFaceId().map((d) => d.name).join(", ") })
                  : t("approvals.faceIdNone")}
              </p>
              <Show when={error() === "save"}>
                <p class={props.desktop ? "pg-notice" : "form-error"} role="alert">
                  {t("approvals.saveError")}
                </p>
              </Show>
              <Show when={saved()}>
                <p class={props.desktop ? "pg-muted" : "muted small"} role="status">
                  {t("approvals.saved")}
                </p>
              </Show>
            </>
          )}
        </Show>
      </div>
  );

  // Desktop: the settings page shows it as a Card of its own.
  return (
    <Show when={settings()}>
      <Show when={props.desktop} fallback={form()}>
        <Card title={t("approvals.settingsTitle")}>{form()}</Card>
      </Show>
    </Show>
  );
}
