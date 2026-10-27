import { createSignal, Match, onCleanup, onMount, Switch } from "solid-js";
import { toApiError } from "../api/client";
import { api } from "../api/secretary";
import { t } from "../i18n";
import { checkSession } from "../state/session";

// QR sign-in on the signed-out screen, like Discord's: ask the server for a code, show it as a QR
// code, and poll every 2 s while the page is visible. A phone where the owner is signed in scans
// it (/approve?c=…) and approves; the next poll sets the session cookie and the app opens.
// An expired code is replaced on its own, up to MAX_CODES times; after that a button asks again,
// so a forgotten tab does not keep asking forever.

export const POLL_MS = 2_000;
export const MAX_CODES = 5;

type Phase =
  | { kind: "loading" }
  | { kind: "ready"; code: string; pollToken: string; expiresAt: number }
  | { kind: "approved" }
  | { kind: "denied" }
  | { kind: "paused" }
  | { kind: "unavailable" };

export default function QrLogin() {
  const [phase, setPhase] = createSignal<Phase>({ kind: "loading" });
  let codes = 0;
  let busy = false;
  let retryAt = 0;
  let disposed = false;
  let timer: ReturnType<typeof setInterval> | undefined;

  async function start(byHand = false) {
    if (byHand) codes = 0;
    if (codes >= MAX_CODES) {
      setPhase({ kind: "paused" });
      return;
    }
    codes += 1;
    busy = true;
    if (phase().kind !== "ready") setPhase({ kind: "loading" });
    try {
      const r = await api.qrStart();
      if (disposed) return;
      setPhase({ kind: "ready", code: r.code, pollToken: r.pollToken, expiresAt: Date.now() + r.expiresIn * 1000 });
    } catch (e) {
      if (disposed) return;
      const err = toApiError(e);
      // Rate limited: the password form shares the budget; try again when the server says.
      if (err.status === 429) {
        codes -= 1;
        retryAt = Date.now() + (err.retryAfterSeconds && err.retryAfterSeconds > 0 ? err.retryAfterSeconds : 60) * 1000;
        setPhase({ kind: "loading" });
      } else {
        setPhase({ kind: "unavailable" });
      }
    } finally {
      busy = false;
    }
  }

  async function tick() {
    if (busy || disposed || document.visibilityState === "hidden") return;
    const p = phase();
    if (p.kind === "loading") {
      if (retryAt && Date.now() >= retryAt) {
        retryAt = 0;
        await start();
      }
      return;
    }
    if (p.kind !== "ready") return;
    if (Date.now() >= p.expiresAt) {
      await start();
      return;
    }
    busy = true;
    try {
      const { status } = await api.qrPoll(p.pollToken);
      if (disposed || phase() !== p) return;
      if (status === "approved") {
        stop();
        setPhase({ kind: "approved" });
        await checkSession();
      } else if (status === "denied") {
        setPhase({ kind: "denied" });
      } else if (status === "expired") {
        busy = false;
        await start();
      }
    } catch {
      // Network blip: the next tick polls again.
    } finally {
      busy = false;
    }
  }

  function stop() {
    if (timer !== undefined) clearInterval(timer);
    timer = undefined;
  }

  onMount(() => {
    void start();
    timer = setInterval(() => void tick(), POLL_MS);
  });
  onCleanup(() => {
    disposed = true;
    stop();
  });

  const again = () => void start(true);
  const ready = () => {
    const p = phase();
    return p.kind === "ready" ? p : null;
  };

  return (
    <section class="login-qr" aria-labelledby="login-qr-title">
      <div class="login-qr-frame" aria-busy={phase().kind === "loading"}>
        <Switch>
          <Match when={ready()}>
            {(p) => <img class="login-qr-image" src={api.qrImageUrl(p().code)} alt={t("login.qrAlt")} width="180" height="180" />}
          </Match>
          <Match when={phase().kind === "loading"}>
            <p class="login-qr-note" role="status">
              {t("login.qrLoading")}
            </p>
          </Match>
          <Match when={phase().kind === "approved"}>
            <p class="login-qr-note login-qr-note--ok" role="status">
              {t("login.qrApproved")}
            </p>
          </Match>
          <Match when={phase().kind === "denied" || phase().kind === "paused" || phase().kind === "unavailable"}>
            <div class="login-qr-note">
              <p>
                {phase().kind === "denied" ? t("login.qrDenied") : phase().kind === "paused" ? t("login.qrPaused") : t("login.qrUnavailable")}
              </p>
              <button type="button" class="btn btn--small" onClick={again}>
                {t("login.qrNewCode")}
              </button>
            </div>
          </Match>
        </Switch>
      </div>
      <h2 id="login-qr-title" class="login-qr-title">
        {t("login.qrScan")}
      </h2>
      <p class="login-qr-hint">{t("login.qrHint")}</p>
    </section>
  );
}
