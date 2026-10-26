import { createMemo, createSignal, onCleanup, Show } from "solid-js";
import { toApiError, type ApiError } from "../api/client";
import { t } from "../i18n";
import { formatCountdown } from "../lib/units";
import { signIn } from "../state/session";

// Single-user sign-in: email + password (POST /v1/session). The field
// attributes let iCloud Keychain and browser password managers fill it.


/** Message for a failed sign-in; 429s are handled by the countdown instead. */
export function loginErrorMessage(err: ApiError): string {
  if (err.status === 401) return t("login.invalid");
  if (err.isNetwork) return t("login.unreachable");
  return err.message || t("login.failed");
}

export default function Login() {
  const [email, setEmail] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [showPassword, setShowPassword] = createSignal(false);
  const [pending, setPending] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  // Lockout: which kind and when it ends (local clock), for the countdown.
  const [lock, setLock] = createSignal<{ code: string; until: number } | null>(null);
  const [now, setNow] = createSignal(Date.now());
  let emailInput: HTMLInputElement | undefined;
  let passwordInput: HTMLInputElement | undefined;
  let timer: ReturnType<typeof setInterval> | undefined;

  const secondsLeft = createMemo(() => {
    const l = lock();
    return l ? Math.max(0, Math.ceil((l.until - now()) / 1000)) : 0;
  });
  const locked = () => secondsLeft() > 0;

  function stopTimer() {
    if (timer !== undefined) clearInterval(timer);
    timer = undefined;
  }
  onCleanup(stopTimer);

  function startLock(code: string, seconds: number) {
    stopTimer();
    setNow(Date.now());
    setLock({ code, until: Date.now() + seconds * 1000 });
    timer = setInterval(() => {
      setNow(Date.now());
      if (secondsLeft() === 0) {
        stopTimer();
        setLock(null);
      }
    }, 1000);
  }

  async function onSubmit(e: SubmitEvent) {
    e.preventDefault();
    if (pending() || locked()) return;
    const address = email().trim();
    if (!address || !password()) {
      setError(t("login.enterBoth"));
      (address ? passwordInput : emailInput)?.focus();
      return;
    }
    setPending(true);
    setError(null);
    try {
      await signIn(address, password());
    } catch (err) {
      const apiErr = toApiError(err);
      if (apiErr.status === 429) {
        startLock(apiErr.code, apiErr.retryAfterSeconds && apiErr.retryAfterSeconds > 0 ? apiErr.retryAfterSeconds : 60);
      } else {
        setError(loginErrorMessage(apiErr));
        passwordInput?.focus();
        passwordInput?.select();
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <main class="login">
      <div class="login-bg" aria-hidden="true">
        <span class="login-orbit login-orbit--3" />
        <span class="login-orbit login-orbit--1" />
        <span class="login-orbit login-orbit--2" />
      </div>
      <form class="login-card" onSubmit={onSubmit} novalidate>
        <div class="login-mark" aria-hidden="true">
          <img src="/icon-192.png" alt="" width="60" height="60" />
        </div>
        <p class="login-eyebrow" aria-hidden="true">
          Lucina · Secretary
        </p>
        <h1 class="login-title">Secretary</h1>
        <p class="login-sub">{t("login.sub")}</p>
        <label class="field-label" for="login-email">
          {t("login.email")}
        </label>
        <input
          ref={emailInput}
          id="login-email"
          name="email"
          class="text-input"
          type="email"
          autocomplete="username"
          autocapitalize="off"
          spellcheck={false}
          inputmode="email"
          value={email()}
          onInput={(e) => setEmail(e.currentTarget.value)}
          aria-invalid={error() ? "true" : "false"}
          disabled={pending()}
          autofocus
        />
        <label class="field-label login-password-label" for="login-password">
          {t("login.password")}
        </label>
        <div class="password-field">
          <input
            ref={passwordInput}
            id="login-password"
            name="password"
            class="text-input"
            type={showPassword() ? "text" : "password"}
            autocomplete="current-password"
            autocapitalize="off"
            spellcheck={false}
            enterkeyhint="go"
            value={password()}
            onInput={(e) => setPassword(e.currentTarget.value)}
            aria-invalid={error() ? "true" : "false"}
            aria-describedby={error() ? "login-error" : undefined}
            disabled={pending()}
          />
          <button
            type="button"
            class="password-toggle"
            aria-pressed={showPassword()}
            aria-controls="login-password"
            onClick={() => setShowPassword((v) => !v)}
          >
            {showPassword() ? t("login.hide") : t("login.show")}
          </button>
        </div>
        <Show when={error()}>
          {(msg) => (
            <p id="login-error" class="form-error" role="alert">
              {msg()}
            </p>
          )}
        </Show>
        <Show when={locked()}>
          <p class="form-error" role="alert">
            {lock()?.code === "account_locked" ? t("login.locked") : t("login.tooMany")}{" "}
            <span class="countdown">{t("login.tryAgainIn", { time: formatCountdown(secondsLeft()) })}</span>
          </p>
        </Show>
        <button class="btn btn--primary btn--block" type="submit" disabled={pending() || locked()}>
          {pending() ? t("login.signingIn") : t("login.signIn")}
        </button>
      </form>
    </main>
  );
}
