import { createEffect, createSignal, on, Show } from "solid-js";
import { toApiError } from "../api/client";
import { api } from "../api/secretary";

export const MIN_PASSWORD_LENGTH = 12;

/** Account panel opened from the header: change the single user's password. */
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
    if (!current()) return setError("Enter your current password.");
    if (next().length < MIN_PASSWORD_LENGTH) {
      return setError(`The new password needs at least ${MIN_PASSWORD_LENGTH} characters.`);
    }
    if (next() !== confirm()) return setError("The new passwords don't match.");
    if (next() === current()) return setError("Choose a password different from the current one.");
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
          ? "Your current password is incorrect."
          : apiErr.status === 429
            ? "Too many attempts. Please wait and try again."
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
              Account
            </h2>
            <button type="button" class="icon-btn" aria-label="Close" onClick={() => props.onClose()}>
              ×
            </button>
          </header>
          <form class="settings-form" onSubmit={submit} novalidate aria-label="Change password">
            <h3 class="field-label">Change password</h3>
            <label class="field-label" for="pw-current">
              Current password
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
              New password
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
              At least {MIN_PASSWORD_LENGTH} characters.
            </p>
            <label class="field-label" for="pw-confirm">
              Confirm new password
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
              Show passwords
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
                Password changed.
              </p>
            </Show>
            <button type="submit" class="btn btn--primary" disabled={saving()}>
              {saving() ? "Saving…" : "Change password"}
            </button>
          </form>
        </div>
      </div>
    </Show>
  );
}
