import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { parseConnection, parseSettings, pixivApi, type PixivConnection, type PixivSettings as Settings } from "../api/pixiv";
import { toApiError } from "../api/client";
import { t, type MessageKey } from "../i18n";
import { showAdult } from "../lib/adult";
import { desktopShell } from "../lib/desktopShell";
import "../styles/pixiv-settings.css";

const errorKeys: Record<string, MessageKey> = {
  approval_denied: "pixiv.approval_denied", approval_expired: "pixiv.approval_expired",
  approval_unavailable: "pixiv.approval_unavailable", save_failed: "pixiv.save_failed",
  remove_denied: "pixiv.remove_denied", remove_failed: "pixiv.remove_failed",
  token_rejected: "pixiv.expired", broker_unavailable: "pixiv.unavailable", broker_unconfigured: "pixiv.broker_unconfigured",
  pixiv_login_failed: "pixiv.pixiv_login_failed", connect_expired: "pixiv.connect_expired", busy: "pixiv.busy",
};
export function PixivSettings(props: { desktop?: boolean }) {
  const [connection, setConnection] = createSignal(parseConnection(null));
  const [settings, setSettings] = createSignal(parseSettings(null));
  const [busy, setBusy] = createSignal(false);
  const [message, setMessage] = createSignal("");
  const [saveMessage, setSaveMessage] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [confirm, setConfirm] = createSignal(false);
  let disposed = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let deadline = 0;
  const waiting = () => connection().store === "waiting_approval" || connection().removal === "waiting_approval";
  function accept(c: PixivConnection) {
    if (disposed) return;
    setConnection(c);
    clearTimeout(timer);
    if (waiting()) {
      deadline ||= Date.now() + 300_000;
      if (Date.now() < deadline) timer = setTimeout(() => void refresh(), Math.min(3000, deadline - Date.now()));
    } else deadline = 0;
  }
  async function refresh() {
    if (disposed || (deadline && Date.now() >= deadline)) return;
    try { accept((await pixivApi.status()).connection); }
    catch { if (!disposed) { setMessage(t("pixiv.unavailable")); accept(connection()); } }
  }
  onMount(async () => {
    try {
      const status = await pixivApi.status();
      if (!disposed) { setSettings(status.settings); accept(status.connection); }
    } catch { if (!disposed) accept(parseConnection(null)); }
  });
  onCleanup(() => { disposed = true; clearTimeout(timer); });
  const failure = (e: unknown) => t(errorKeys[toApiError(e).code] ?? "pixiv.loginFailed");
  async function connect() {
    const login = desktopShell()?.pixivLogin;
    if (!login || busy() || waiting()) return;
    setBusy(true); setMessage("");
    try {
      const start = await pixivApi.connectStart();
      if (disposed) return;
      const result = await login(start.loginUrl);
      if (disposed) return;
      if ("code" in result) accept(await pixivApi.connectFinish(start.state, result.code));
      else if (result.error !== "cancelled") setMessage(t("pixiv.loginFailed"));
    } catch (e) { if (!disposed) setMessage(failure(e)); }
    finally { if (!disposed) setBusy(false); }
  }
  async function disconnect() {
    setConfirm(false); setBusy(true); setMessage("");
    try { accept(await pixivApi.disconnect()); }
    catch (e) { if (!disposed) setMessage(t(errorKeys[toApiError(e).code] ?? "pixiv.remove_failed")); }
    finally { if (!disposed) setBusy(false); }
  }
  async function save(patch: Partial<Settings>) {
    if (saving()) return;
    setSaving(true); setSaveMessage("");
    try { const saved = await pixivApi.saveSettings(patch); if (!disposed) { setSettings(saved); setSaveMessage(t("pixiv.saved")); } }
    catch { if (!disposed) setSaveMessage(t("pixiv.settingsError")); }
    finally { if (!disposed) setSaving(false); }
  }
  const statusText = () => {
    const c = connection();
    if (c.removal === "waiting_approval") return t("pixiv.removalWaiting");
    if (c.store === "waiting_approval") return t("pixiv.storeWaiting");
    if (c.error && errorKeys[c.error]) return t(errorKeys[c.error]!);
    if (c.state === "connected") return t("pixiv.connected", { name: c.user?.name ?? "", account: c.user?.account ?? "", premium: t(c.premium ? "pixiv.yes" : "pixiv.no") });
    return t(c.state === "disconnected" ? "pixiv.disconnected" : c.state === "expired" ? "pixiv.expired" : "pixiv.unavailable");
  };
  const buttonClass = () => props.desktop ? "pg-btn" : "btn";
  return <section class="pixiv-settings" aria-labelledby="pixiv-settings-title">
    <h3 id="pixiv-settings-title">{t("pixiv.title")}</h3>
    <p role="status">{statusText()}</p>
    <Show when={message()}><p role="alert">{message()}</p></Show>
    <Show when={desktopShell()?.pixivLogin} fallback={<p>{t("pixiv.browserHint")}</p>}>
      <button type="button" class={buttonClass()} disabled={busy() || waiting()} onClick={() => void connect()}>{t(connection().state === "connected" || connection().state === "expired" ? "pixiv.reconnect" : "pixiv.connect")}</button>
    </Show>
    <p class="muted small">{t("pixiv.loginHint")}</p>
    <Show when={connection().state === "connected" || connection().state === "expired" || connection().store === "saved"}>
      <button type="button" class={buttonClass()} disabled={busy() || waiting()} onClick={() => setConfirm(true)}>{t("pixiv.disconnect")}</button>
    </Show>
    <Show when={confirm()}><div role="group" aria-label={t("pixiv.disconnect")}>
      <p>{t("pixiv.disconnectConfirm")}</p>
      <button type="button" class={buttonClass()} onClick={() => void disconnect()}>{t("pixiv.disconnect")}</button>
      <button type="button" class={buttonClass()} onClick={() => setConfirm(false)}>{t("common.cancel")}</button>
    </div></Show>
    <Show when={connection().state === "connected" && connection().r18 === false}>
      <p>{t("pixiv.r18Notice")}</p>
      <a class="pg-link" href="https://www.pixiv.net/settings/viewing" target="_blank" rel="noopener">{t("pixiv.openSettings")}</a>
    </Show>
    <span id="pixiv-default-content" class="field-label">{t("pixiv.defaultContent")}</span>
    <div class={props.desktop ? "pg-choices" : "segmented segmented--wide"} role="group" aria-labelledby="pixiv-default-content">
      <For each={["safe", "r18", "all"] as const}>{value => <button type="button" class={props.desktop ? "pg-btn" : "segmented-btn"} aria-pressed={settings().defaultContent === value} disabled={saving() || (value !== "safe" && !showAdult())} onClick={() => void save({ defaultContent: value })}>{t(value === "safe" ? "pixiv.safetySafe" : value === "r18" ? "pixiv.safetyR18" : "pixiv.safetyAll")}</button>}</For>
    </div>
    <Show when={!showAdult()}><p class="muted small">{t("pixiv.adultHint")}</p></Show>
    <span id="pixiv-default-sort" class="field-label">{t("pixiv.defaultSort")}</span>
    <div class={props.desktop ? "pg-choices" : "segmented segmented--wide"} role="group" aria-labelledby="pixiv-default-sort">
      <For each={["date_d", "popular"] as const}>{value => <button type="button" class={props.desktop ? "pg-btn" : "segmented-btn"} aria-pressed={settings().defaultSort === value} disabled={saving()} onClick={() => void save({ defaultSort: value })}>{t(value === "popular" ? "pixiv.orderPopular" : "pixiv.defaultNewest")}</button>}</For>
    </div>
    <Show when={saveMessage()}><p class="muted small" role="status">{saveMessage()}</p></Show>
  </section>;
}
