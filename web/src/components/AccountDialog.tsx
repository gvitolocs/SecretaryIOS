import { createEffect, createSignal, on, Show } from "solid-js";
import { toApiError } from "../api/client";
import { api } from "../api/secretary";
import { detectedLocale, LOCALE_NAMES, LOCALES, localeOverride, setLocale, t, type Locale } from "../i18n";

export const MIN_PASSWORD_LENGTH = 8;

/** Account panel opened from the header: language, and the single user's password. */
export function AccountDialog(props: { open: boolean; onClose: () => void }) {
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
      () => props.open,
      (open) => {
        if (!open) return;
        setCurrent("");
        setNext("");
        setConfirm("");
        setError(null);
        setDone(false);
        queueMicrotask(() => firstInput?.focus());
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

  return (
    <Show when={props.open}>
      <div class="picker-overlay" onClick={(e) => e.target === e.currentTarget && props.onClose()}>
        <div
          class="picker-sheet account-sheet"
          role="dialog"
          aria-modal="true"
          aria-labelledby="account-title"
          onKeyDown={(e) => {
            if (e.key === "Escape") props.onClose();
          }}
        >
          <header class="picker-head">
            <h2 id="account-title" class="section-title">
              {t("shell.account")}
            </h2>
            <button type="button" class="icon-btn" aria-label={t("common.close")} onClick={() => props.onClose()}>
              ×
            </button>
          </header>
          <div class="settings-form">
            <label class="field-label" for="account-language">
              {t("shell.language")}
            </label>
            <select
              id="account-language"
              class="text-input"
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
          <form class="settings-form" onSubmit={submit} novalidate aria-label={t("shell.changePassword")}>
            <h3 class="field-label">{t("shell.changePassword")}</h3>
            <label class="field-label" for="pw-current">
              {t("shell.currentPassword")}
            </label>
            <input
              ref={firstInput}
              id="pw-current"
              class="text-input"
              type={type()}
              autocomplete="current-password"
              value={current()}
              onInput={(e) => setCurrent(e.currentTarget.value)}
              disabled={saving()}
            />
            <label class="field-label" for="pw-new">
              {t("shell.newPassword")}
            </label>
            <input
              id="pw-new"
              class="text-input"
              type={type()}
              autocomplete="new-password"
              minlength={MIN_PASSWORD_LENGTH}
              value={next()}
              onInput={(e) => setNext(e.currentTarget.value)}
              aria-describedby="pw-hint"
              disabled={saving()}
            />
            <p id="pw-hint" class="muted small">
              {t("shell.passwordHint", { count: MIN_PASSWORD_LENGTH })}
            </p>
            <label class="field-label" for="pw-confirm">
              {t("shell.confirmPassword")}
            </label>
            <input
              id="pw-confirm"
              class="text-input"
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
                <p class="form-error" role="alert">
                  {msg()}
                </p>
              )}
            </Show>
            <Show when={done()}>
              <p class="muted" role="status">
                {t("shell.passwordChanged")}
              </p>
            </Show>
            <button type="submit" class="btn btn--primary" disabled={saving()}>
              {saving() ? t("common.saving") : t("shell.changePassword")}
            </button>
          </form>
        </div>
      </div>
    </Show>
  );
}
