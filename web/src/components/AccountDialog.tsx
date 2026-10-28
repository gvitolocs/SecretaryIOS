import { createEffect, createSignal, on, Show, type JSX } from "solid-js";
import { Button, Card, Page, PageGrid, PageHeader, SectionNav } from "./page";
import { session } from "../state/session";
import { toApiError } from "../api/client";
import { api } from "../api/secretary";
import { openWardrobe } from "../state/wardrobe";
import { HangerIcon, QrIcon } from "./Icons";
import { ApprovalSettings } from "./ApprovalSettings";
import { KeepImport } from "./KeepImport";
import { NotificationSettings } from "./NotificationSettings";
import { detectedLocale, LOCALE_NAMES, LOCALES, localeOverride, setLocale, t, type Locale, type MessageKey } from "../i18n";
import {
  QUALITIES,
  setStageQuality,
  setThemeSetting,
  stageQuality,
  THEMES,
  themeSetting,
  type StageQualitySetting,
  type ThemeSetting,
} from "../state/appearance";

const THEME_LABELS: Record<ThemeSetting, MessageKey> = {
  system: "shell.themeSystem",
  dark: "shell.themeDark",
  light: "shell.themeLight",
};
const QUALITY_LABELS: Record<StageQualitySetting, MessageKey> = {
  auto: "shell.qualityAuto",
  low: "shell.qualityLow",
  medium: "shell.qualityMedium",
  high: "shell.qualityHigh",
};

export const MIN_PASSWORD_LENGTH = 8;

/** Account panel opened from the header: language, and the single user's password. */
export function AccountDialog(props: {
  open: boolean;
  desktop?: boolean;
  onClose: () => void;
  section?: "notifications" | "import" | "approvals" | null;
  /** A sign-out button at the end (the /account route; the top bar has its own). */
  onSignOut?: () => void;
  signingOut?: boolean;
}) {
  const part = () => props.section ?? "account";
  const deviceName = () => { const s = session(); return s.status === "authed" ? s.deviceName : null; };
  const [current, setCurrent] = createSignal("");
  const [next, setNext] = createSignal("");
  const [confirm, setConfirm] = createSignal("");
  const [show, setShow] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [done, setDone] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  let firstInput: HTMLInputElement | undefined;

  createEffect(
    on(
      () => [props.open, props.section] as const,
      ([open]) => {
        if (!open) return;
        setCurrent("");
        setNext("");
        setConfirm("");
        setError(null);
        setDone(false);
        if (props.desktop) return;
        queueMicrotask(() => {
          if (props.section === "import") {
            document.getElementById("kp-import")?.scrollIntoView?.({ block: "start" });
          } else if (props.section === "approvals") {
            document.getElementById("apv-settings-title")?.scrollIntoView?.({ block: "start" });
          } else if (props.section === "notifications") {
            const el = document.getElementById("notif-settings-title");
            el?.scrollIntoView?.({ block: "start" });
            (el?.parentElement?.querySelector("button, input") as HTMLElement | null)?.focus();
          } else {
            firstInput?.focus();
          }
        });
      },
    ),
  );

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (saving()) return;
    if (!current()) return setError(t("shell.errEnterCurrent"));
    if (next().length < MIN_PASSWORD_LENGTH) {
      return setError(t("shell.errTooShort", { count: MIN_PASSWORD_LENGTH }));
    }
    if (next() !== confirm()) return setError(t("shell.errMismatch"));
    if (next() === current()) return setError(t("shell.errSame"));
    setError(null);
    setSaving(true);
    try {
      await api.changePassword(current(), next());
      setDone(true);
      setCurrent("");
      setNext("");
      setConfirm("");
    } catch (err) {
      const apiErr = toApiError(err);
      // The server answers a wrong current password with 403 invalid_credentials
      // (the caller stays signed in) and repeated failures with 429 account_locked.
      setError(
        apiErr.code === "invalid_credentials"
          ? t("shell.errWrongCurrent")
          : apiErr.status === 429
            ? t("shell.errTooMany")
            : apiErr.message,
      );
    } finally {
      setSaving(false);
    }
  }

  const type = () => (show() ? "text" : "password");

  // Phone: the dialog's sections one after another. Desktop: one section of the page at a time, each a Card.
  function Section(p: { id: "account" | "notifications" | "import"; title: string; children: JSX.Element }) {
    return <Show when={!props.desktop || part() === p.id} fallback={null}>
      <Show when={props.desktop} fallback={p.children}><Card title={p.title}>{p.children}</Card></Show>
    </Show>;
  }
  function Frame(p: { children: JSX.Element }) {
    return <Show when={props.desktop} fallback={
      <div class="picker-overlay" onClick={e => e.target === e.currentTarget && props.onClose()}>
        <div class="picker-sheet account-sheet" role="dialog" aria-modal="true" aria-labelledby="account-title"
          onKeyDown={e => { if (e.key === "Escape") props.onClose(); }}>
          <header class="picker-head"><h2 id="account-title" class="section-title">{t("shell.account")}</h2>
            <button type="button" class="icon-btn" aria-label={t("common.close")} onClick={() => props.onClose()}>×</button>
          </header>
          {p.children}
          <Show when={props.onSignOut}>{fn => <div class="settings-form"><button type="button" class="btn" disabled={props.signingOut} onClick={() => fn()()}>{t("shell.logout")}</button></div>}</Show>
        </div>
      </div>
    }>
      <Page>
        <PageHeader title={t("shell.account")} context={deviceName()} nav={<SectionNav label={t("shell.accountSections")} items={[
          { label: t("shell.general"), href: "/account", current: part() === "account" },
          { label: t("notify.settings"), href: "/account?section=notifications", current: part() === "notifications" },
          { label: t("keep.importTitle"), href: "/account?section=import", current: part() === "import" },
        ]} />} />
        <PageGrid layout="rail" rail={<Card label={t("shell.account")}>
          <p class="pg-strong">{deviceName() ?? t("shell.account")}</p>
          <Show when={props.onSignOut}>{fn => <Button disabled={props.signingOut} onClick={() => fn()()}>{t("shell.logout")}</Button>}</Show>
        </Card>}>{p.children}</PageGrid>
      </Page>
    </Show>;
  }
  return <Show when={props.open}><Frame>
          <Section id="account" title={t("shell.language")}>
          <div class={props.desktop ? "pg-form" : "settings-form"}>
            <label class={props.desktop ? "pg-field-label" : "field-label"} for="account-language">
              {t("shell.language")}
            </label>
            <select
              id="account-language"
              class={props.desktop ? "pg-input" : "text-input"}
              value={localeOverride() ?? "auto"}
              onChange={(e) => {
                const v = e.currentTarget.value;
                setLocale((LOCALES as readonly string[]).includes(v) ? (v as Locale) : null);
              }}
            >
              <option value="auto">{t("shell.languageAuto", { name: LOCALE_NAMES[detectedLocale()] })}</option>
              {LOCALES.map((l) => (
                <option value={l} lang={l}>
                  {LOCALE_NAMES[l]}
                </option>
              ))}
            </select>
          </div>
          </Section>
          <Section id="account" title={t("shell.appearance")}>
          <div class={props.desktop ? "pg-form" : "settings-form appearance"}>
            <Show when={!props.desktop}><h3 class={props.desktop ? "pg-field-label" : "field-label"}>{t("shell.appearance")}</h3></Show>
            <span class={props.desktop ? "pg-field-label" : "field-label appearance-label"} id="appearance-theme">
              {t("shell.theme")}
            </span>
            <div class={props.desktop ? "pg-choices" : "segmented segmented--wide"} role="group" aria-labelledby="appearance-theme">
              {THEMES.map((v) => (
                <button
                  type="button"
                  class={props.desktop ? "pg-btn" : "segmented-btn"}
                  classList={{ "segmented-btn--active": themeSetting() === v }}
                  aria-pressed={themeSetting() === v ? "true" : "false"}
                  onClick={() => setThemeSetting(v)}
                >
                  {t(THEME_LABELS[v])}
                </button>
              ))}
            </div>
            <span class={props.desktop ? "pg-field-label" : "field-label appearance-label"} id="appearance-quality">
              {t("shell.stageQuality")}
            </span>
            <div class={props.desktop ? "pg-choices" : "segmented segmented--wide"} role="group" aria-labelledby="appearance-quality" aria-describedby="appearance-quality-hint">
              {QUALITIES.map((v) => (
                <button
                  type="button"
                  class={props.desktop ? "pg-btn" : "segmented-btn"}
                  classList={{ "segmented-btn--active": stageQuality() === v }}
                  aria-pressed={stageQuality() === v ? "true" : "false"}
                  onClick={() => setStageQuality(v)}
                >
                  {t(QUALITY_LABELS[v])}
                </button>
              ))}
            </div>
            <p id="appearance-quality-hint" class={props.desktop ? "pg-muted" : "muted small"}>
              {t("shell.stageQualityHint")}
            </p>
            <button
              type="button"
              class={props.desktop ? "pg-link" : "settings-link"}
              onClick={() => {
                props.onClose();
                openWardrobe();
              }}
            >
              <span class="settings-link-icon" aria-hidden="true">
                <HangerIcon />
              </span>
              <span class="settings-link-text">
                <span class={props.desktop ? "pg-strong" : "settings-link-title"}>{t("wardrobe.open")}</span>
                <span class={props.desktop ? "pg-muted" : "muted small"}>{t("wardrobe.openHint")}</span>
              </span>
              <span class="settings-link-chevron" aria-hidden="true">
                ›
              </span>
            </button>
          </div>
          </Section>
          <Section id="account" title={t("login.scanOpen")}>
          <div class={props.desktop ? "pg-form" : "settings-form"}>
            {/* A plain link: the router handles it, and it needs no router in tests. */}
            <a class={props.desktop ? "pg-link" : "settings-link"} href="/scan" onClick={() => props.onClose()}>
              <span class="settings-link-icon" aria-hidden="true">
                <QrIcon />
              </span>
              <span class="settings-link-text">
                <span class={props.desktop ? "pg-strong" : "settings-link-title"}>{t("login.scanOpen")}</span>
                <span class={props.desktop ? "pg-muted" : "muted small"}>{t("login.scanOpenHint")}</span>
              </span>
              <span class="settings-link-chevron" aria-hidden="true">
                ›
              </span>
            </a>
          </div>
          </Section>
          <Section id="notifications" title={t("notify.settings")}><NotificationSettings desktop={props.desktop} /></Section>
          <Show when={!props.desktop || part() === "notifications"}><ApprovalSettings desktop={props.desktop} /></Show>
          <Section id="import" title={t("keep.importTitle")}><KeepImport desktop={props.desktop} /></Section>
          <Section id="account" title={t("shell.changePassword")}>
          <form class={props.desktop ? "pg-form" : "settings-form"} onSubmit={submit} novalidate aria-label={t("shell.changePassword")}>
            <Show when={!props.desktop}><h3 class={props.desktop ? "pg-field-label" : "field-label"}>{t("shell.changePassword")}</h3></Show>
            <label class={props.desktop ? "pg-field-label" : "field-label"} for="pw-current">
              {t("shell.currentPassword")}
            </label>
            <input
              ref={firstInput}
              id="pw-current"
              class={props.desktop ? "pg-input" : "text-input"}
              type={type()}
              autocomplete="current-password"
              value={current()}
              onInput={(e) => setCurrent(e.currentTarget.value)}
              disabled={saving()}
            />
            <label class={props.desktop ? "pg-field-label" : "field-label"} for="pw-new">
              {t("shell.newPassword")}
            </label>
            <input
              id="pw-new"
              class={props.desktop ? "pg-input" : "text-input"}
              type={type()}
              autocomplete="new-password"
              minlength={MIN_PASSWORD_LENGTH}
              value={next()}
              onInput={(e) => setNext(e.currentTarget.value)}
              aria-describedby="pw-hint"
              disabled={saving()}
            />
            <p id="pw-hint" class={props.desktop ? "pg-muted" : "muted small"}>
              {t("shell.passwordHint", { count: MIN_PASSWORD_LENGTH })}
            </p>
            <label class={props.desktop ? "pg-field-label" : "field-label"} for="pw-confirm">
              {t("shell.confirmPassword")}
            </label>
            <input
              id="pw-confirm"
              class={props.desktop ? "pg-input" : "text-input"}
              type={type()}
              autocomplete="new-password"
              value={confirm()}
              onInput={(e) => setConfirm(e.currentTarget.value)}
              disabled={saving()}
            />
            <label class="check-row">
              <input type="checkbox" checked={show()} onChange={(e) => setShow(e.currentTarget.checked)} />
              {t("shell.showPasswords")}
            </label>
            <Show when={error()}>
              {(msg) => (
                <p class={props.desktop ? "pg-notice" : "form-error"} role="alert">
                  {msg()}
                </p>
              )}
            </Show>
            <Show when={done()}>
              <p class="muted" role="status">
                {t("shell.passwordChanged")}
              </p>
            </Show>
            <button type="submit" class={props.desktop ? "pg-btn" : "btn btn--primary"} disabled={saving()}>
              {saving() ? t("common.saving") : t("shell.changePassword")}
            </button>
          </form>
          </Section>
  </Frame></Show>;
}
