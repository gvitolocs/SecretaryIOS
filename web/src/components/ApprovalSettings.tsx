// "Approvals" in the account panel: which device gets owner-approval requests first. With none picked, every
// device of yours sees a request at once. It also says which devices have Face ID set up (needed for sensitive
// requests). Inside Secretary Code it also sets this Mac up for Touch ID approvals, and lets a Mac that has a key
// revoke another device's. Shown only when the server has the approvals routes.

import { createEffect, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { approvalsApi, type ApprovalDevice, type ApprovalSettings as Settings } from "../api/approvals";
import { desktopApprover, type ApproverFailure } from "../api/desktopApprover";
import { speechLang, t, type MessageKey } from "../i18n";
import { macApproverStatus, refreshMacApprover } from "../state/macApprover";

/** While this Mac waits for the iPhone to approve it, look at the server this often. */
export const PENDING_POLL_MS = 3_000;

const when = (iso: string) => {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleDateString(speechLang(), { year: "numeric", month: "short", day: "numeric" });
};

export function ApprovalSettings() {
  const [settings, setSettings] = createSignal<Settings | null>(null);
  const [saving, setSaving] = createSignal(false);
  const [saved, setSaved] = createSignal(false);
  const [error, setError] = createSignal<"save" | null>(null);
  // This Mac (Secretary Code only).
  const [mac, setMac] = createSignal<"setup" | "remove" | null>(null);
  const [macNote, setMacNote] = createSignal<MessageKey | null>(null);
  const [justPending, setJustPending] = createSignal(false);
  // Another device being revoked: asking first, then busy.
  const [revoking, setRevoking] = createSignal<{ id: string; busy: boolean } | null>(null);
  const [revokeError, setRevokeError] = createSignal(false);
  const bridge = desktopApprover();

  /** Read the settings again. Quiet on failure: what is on screen stays. */
  async function reload() {
    try {
      const next = await approvalsApi.settings();
      if (next) setSettings(next);
    } catch {
      // Keep what is shown.
    }
  }

  onMount(async () => {
    if (bridge) void refreshMacApprover();
    try {
      setSettings(await approvalsApi.settings());
    } catch {
      // 404: the server doesn't have approvals yet; any other failure: the section just isn't offered. Say nothing.
    }
  });

  const thisDevice = (): ApprovalDevice | undefined => settings()?.devices.find((d) => d.id === settings()?.thisDeviceId);
  /** This Mac asked to become an approver and the iPhone has not approved yet. */
  const waiting = () => {
    const d = thisDevice();
    return !!bridge && !d?.hasBiometricKey && (justPending() || !!d?.pendingApprovalId);
  };
  // Poll until the iPhone approves (the key turns active) or the request goes away.
  createEffect(() => {
    if (!waiting()) return;
    const timer = setInterval(() => void reload(), PENDING_POLL_MS);
    onCleanup(() => clearInterval(timer));
  });
  createEffect(() => {
    if (thisDevice()?.hasBiometricKey) setJustPending(false);
  });

  /** A bridge call failed: nothing for a cancelled prompt, otherwise the note for `generic`. */
  function failed(r: ApproverFailure, generic: MessageKey): MessageKey | null {
    if (r.error === "cancelled") return null;
    if (r.error === "unavailable") return "approvals.macUnavailable";
    return generic;
  }

  async function setupMac() {
    if (!bridge || mac()) return;
    setMac("setup");
    setMacNote(null);
    try {
      const r = await bridge.setup();
      if (!r.ok) setMacNote(failed(r, "approvals.errMacSetup"));
      else setJustPending(r.status === "pending_approval");
    } catch {
      setMacNote("approvals.errMacSetup");
    }
    await Promise.all([refreshMacApprover(), reload()]);
    setMac(null);
  }

  async function removeMac() {
    if (!bridge || mac()) return;
    setMac("remove");
    setMacNote(null);
    try {
      const r = await bridge.remove();
      if (!r.ok) setMacNote(failed(r, "approvals.errMacRemove"));
      else setJustPending(false);
    } catch {
      setMacNote("approvals.errMacRemove");
    }
    await Promise.all([refreshMacApprover(), reload()]);
    setMac(null);
  }

  async function revoke(id: string) {
    if (!bridge || revoking()?.busy) return;
    setRevoking({ id, busy: true });
    setRevokeError(false);
    try {
      const r = await bridge.revoke(id);
      if (r.ok) setRevoking(null);
      else {
        setRevoking(null);
        if (r.error !== "cancelled") setRevokeError(true);
      }
    } catch {
      setRevoking(null);
      setRevokeError(true);
    }
    await Promise.all([refreshMacApprover(), reload()]);
  }

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
  // Devices with a key (or one on the way) get a line of their own; older servers send neither.
  const keyed = () => settings()?.devices.filter((d) => d.keyLabel || d.pendingApprovalId) ?? [];
  const macReady = () => {
    const s = macApproverStatus();
    return !!bridge && !!s && s.available && s.hasKey;
  };

  return (
    <Show when={settings()}>
      <div class="settings-form" aria-labelledby="apv-settings-title">
        <h3 class="field-label" id="apv-settings-title">
          {t("approvals.settingsTitle")}
        </h3>
        <Show when={settings()}>
          {(cur) => (
            <>
              <label class="field-label" for="apv-main-device">
                {t("approvals.mainLabel")}
              </label>
              <select
                id="apv-main-device"
                class="text-input"
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
              <p id="apv-main-hint" class="muted small">
                {t("approvals.mainHint", { secs: cur().escalateAfterSecs })}
              </p>
              <p class="muted small" data-testid="apv-faceid-devices">
                {withFaceId().length
                  ? t("approvals.faceIdOn", { names: withFaceId().map((d) => d.name).join(", ") })
                  : t("approvals.faceIdNone")}
              </p>
              <Show when={bridge}>
                <section class="apv-mac" aria-labelledby="apv-mac-title" data-testid="apv-mac">
                  <h4 class="field-label" id="apv-mac-title">
                    {t("approvals.macTitle")}
                  </h4>
                  <Show when={macApproverStatus()}>
                    {(status) => (
                      <Show
                        when={status().available}
                        fallback={
                          <p class="muted small">{t("approvals.macUnavailable")}</p>
                        }
                      >
                        <Show
                          when={thisDevice()?.hasBiometricKey && status().hasKey}
                          fallback={
                            <Show
                              when={waiting()}
                              fallback={
                                <>
                                  <p id="apv-mac-hint" class="muted small">
                                    {t("approvals.macHint")}
                                  </p>
                                  <button
                                    type="button"
                                    class="btn btn--primary"
                                    disabled={mac() !== null}
                                    aria-describedby="apv-mac-hint"
                                    onClick={() => void setupMac()}
                                  >
                                    {mac() === "setup" ? t("approvals.macSettingUp") : t("approvals.macSetup")}
                                  </button>
                                </>
                              }
                            >
                              <p class="muted small" role="status" data-testid="apv-mac-pending">
                                {t("approvals.macPending", { name: thisDevice()?.name ?? "" })}
                              </p>
                            </Show>
                          }
                        >
                          <p class="small" role="status">
                            {t("approvals.macOn")}
                          </p>
                          <button type="button" class="btn" disabled={mac() !== null} onClick={() => void removeMac()}>
                            {mac() === "remove" ? t("approvals.macTurningOff") : t("approvals.macOff")}
                          </button>
                        </Show>
                      </Show>
                    )}
                  </Show>
                  <Show when={macNote()}>
                    {(key) => (
                      <p class="form-error" role="alert">
                        {t(key())}
                      </p>
                    )}
                  </Show>
                </section>
              </Show>
              <Show when={keyed().length > 0}>
                <ul class="apv-devices" aria-label={t("approvals.devicesTitle")}>
                  <For each={keyed()}>
                    {(d) => (
                      <li class="apv-device" data-testid={`apv-device-${d.id}`}>
                        <span class="apv-device-main">
                          <span class="apv-device-name">{d.name}</span>
                          <span class="muted small">
                            {d.pendingApprovalId && !d.hasBiometricKey
                              ? t("approvals.keyWaiting")
                              : [d.keyLabel, d.keyCreatedAt ? t("approvals.keyAdded", { date: when(d.keyCreatedAt) }) : ""]
                                  .filter(Boolean)
                                  .join(", ")}
                          </span>
                        </span>
                        <Show when={macReady() && d.hasBiometricKey && d.id !== cur().thisDeviceId}>
                          <Show
                            when={revoking()?.id === d.id}
                            fallback={
                              <button type="button" class="btn" disabled={revoking()?.busy} onClick={() => setRevoking({ id: d.id, busy: false })}>
                                {t("approvals.revoke")}
                              </button>
                            }
                          >
                            <span class="apv-device-confirm" role="group" aria-label={t("approvals.revokeConfirm", { name: d.name })}>
                              <span class="small">{t("approvals.revokeConfirm", { name: d.name })}</span>
                              <button type="button" class="btn btn--danger" disabled={revoking()?.busy} onClick={() => void revoke(d.id)}>
                                {revoking()?.busy ? t("approvals.revoking") : t("approvals.revoke")}
                              </button>
                              <button type="button" class="btn" disabled={revoking()?.busy} onClick={() => setRevoking(null)}>
                                {t("approvals.revokeCancel")}
                              </button>
                            </span>
                          </Show>
                        </Show>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
              <Show when={revokeError()}>
                <p class="form-error" role="alert">
                  {t("approvals.errRevoke")}
                </p>
              </Show>
              <Show when={error() === "save"}>
                <p class="form-error" role="alert">
                  {t("approvals.saveError")}
                </p>
              </Show>
              <Show when={saved()}>
                <p class="muted small" role="status">
                  {t("approvals.saved")}
                </p>
              </Show>
            </>
          )}
        </Show>
      </div>
    </Show>
  );
}
