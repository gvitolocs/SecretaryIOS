import { createSignal, Show } from "solid-js";
import { toApiError, type ApiError } from "../api/client";
import { signIn } from "../state/session";

export function loginErrorMessage(err: ApiError): string {
  if (err.status === 401) return "That access key wasn't accepted. Check it and try again.";
  if (err.status === 429) {
    const wait = err.retryAfterSeconds;
    return wait && wait > 0
      ? `Too many attempts. Please wait ${wait} second${wait === 1 ? "" : "s"} and try again.`
      : "Too many attempts. Please wait a minute and try again.";
  }
  if (err.isNetwork) return "Can't reach the server. Check your connection and try again.";
  return err.message || "Sign-in failed. Please try again.";
}

export default function Login() {
  const [key, setKey] = createSignal("");
  const [pending, setPending] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  let input: HTMLInputElement | undefined;

  async function onSubmit(e: SubmitEvent) {
    e.preventDefault();
    const token = key().trim();
    if (!token) {
      setError("Enter your access key.");
      input?.focus();
      return;
    }
    if (pending()) return;
    setPending(true);
    setError(null);
    try {
      await signIn(token);
    } catch (err) {
      setError(loginErrorMessage(toApiError(err)));
      input?.focus();
      input?.select();
    } finally {
      setPending(false);
    }
  }

  return (
    <main class="login">
      <form class="login-card" onSubmit={onSubmit} novalidate>
        <div class="login-mark" aria-hidden="true">
          S
        </div>
        <h1 class="login-title">Secretary</h1>
        <p class="login-sub">Enter your access key to continue.</p>
        <label class="field-label" for="access-key">
          Access key
        </label>
        <input
          ref={input}
          id="access-key"
          name="access-key"
          class="text-input"
          type="password"
          autocomplete="off"
          autocapitalize="off"
          spellcheck={false}
          enterkeyhint="go"
          value={key()}
          onInput={(e) => setKey(e.currentTarget.value)}
          aria-invalid={error() ? "true" : "false"}
          aria-describedby={error() ? "login-error" : undefined}
          disabled={pending()}
          autofocus
        />
        <Show when={error()}>
          {(msg) => (
            <p id="login-error" class="form-error" role="alert">
              {msg()}
            </p>
          )}
        </Show>
        <button class="btn btn--primary btn--block" type="submit" disabled={pending()}>
          {pending() ? "Signing in…" : "Sign in"}
        </button>
      </form>
    </main>
  );
}
