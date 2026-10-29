import { A, useSearchParams } from "@solidjs/router";
import { createSignal, Match, onMount, Show, Switch } from "solid-js";
import { toApiError } from "../api/client";
import { api, type QrInfo } from "../api/secretary";
import { locale, t } from "../i18n";
import { describeUserAgent } from "../lib/userAgent";
import { checkSession } from "../state/session";

// /approve?c=… : the other half of QR sign-in (QrLogin.tsx). The QR code on a signed-out screen
// opens this on a device that is already signed in; it shows where the request came from and
// approves or denies it. Signed out, the app shows the normal login first (without a QR code)
// and then this page.

type Phase =
  | { kind: "loading" }
  | { kind: "ready"; info: QrInfo }
  | { kind: "sending"; info: QrInfo; approve: boolean }
  | { kind: "done"; approved: boolean }
  | { kind: "gone" }
  | { kind: "missing" }
  | { kind: "error" };

/** "Safari on macOS", or "Unknown device". */
export function deviceLabel(userAgent: string): string {
  const { browser, os } = describeUserAgent(userAgent);
  if (browser && os) return t("login.approveDevice", { browser, os });
  return browser ?? os ?? t("login.approveUnknown");
}

function clock(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return new Intl.DateTimeFormat(locale(), { hour: "2-digit", minute: "2-digit", second: "2-digit" }).format(d);
}

export default function Approve() {
  const [params] = useSearchParams();
  const code = () => (typeof params.c === "string" ? params.c : "");
  const [phase, setPhase] = createSignal<Phase>({ kind: "loading" });

  // The /v1/session* routes do not trigger the app's lost-session handler, so re-check here.
  async function failed(e: unknown) {
    const err = toApiError(e);
    if (err.status === 401) await checkSession();
    else if (err.status === 404 || err.status === 409) setPhase({ kind: "gone" });
    else setPhase({ kind: "error" });
  }

  async function load() {
    if (!code()) {
      setPhase({ kind: "missing" });
      return;
    }
    setPhase({ kind: "loading" });
    try {
      setPhase({ kind: "ready", info: await api.qrInfo(code()) });
    } catch (e) {
      await failed(e);
    }
  }

  async function answer(approve: boolean) {
    const p = phase();
    if (p.kind !== "ready") return;
    setPhase({ kind: "sending", info: p.info, approve });
    try {
      await api.qrApprove(code(), approve);
      setPhase({ kind: "done", approved: approve });
    } catch (e) {
      await failed(e);
    }
  }

  onMount(() => void load());

  const info = () => {
    const p = phase();
    return p.kind === "ready" || p.kind === "sending" ? p.info : null;
  };
  const sending = () => phase().kind === "sending";

  return (
    <section class="approve-page" aria-labelledby="approve-title">
      <div class="approve-card">
        <h1 id="approve-title" class="approve-title">
          {t("login.approveTitle")}
        </h1>
        <Switch>
          <Match when={phase().kind === "loading"}>
            <p class="approve-sub" role="status">
              {t("login.qrLoading")}
            </p>
          </Match>
          <Match when={info()}>
            {(i) => (
              <>
                <p class="approve-sub">{t("login.approveSub")}</p>
                <dl class="approve-facts">
                  <dt>{t("login.approveDeviceLabel")}</dt>
                  <dd data-testid="approve-device">{deviceLabel(i().userAgent)}</dd>
                  <dt>{t("login.approveIpLabel")}</dt>
                  <dd>
                    <code>{i().ip}</code>
                  </dd>
                  <Show when={clock(i().createdAt)}>
                    <dt>{t("login.approveTimeLabel")}</dt>
                    <dd>{clock(i().createdAt)}</dd>
                  </Show>
                </dl>
                <div class="approve-actions">
                  <button type="button" class="btn" disabled={sending()} onClick={() => void answer(false)}>
                    {t("login.deny")}
                  </button>
                  <button type="button" class="btn btn--primary" disabled={sending()} onClick={() => void answer(true)}>
                    {sending() && (phase() as { approve?: boolean }).approve ? t("login.approving") : t("login.approve")}
                  </button>
                </div>
              </>
            )}
          </Match>
          <Match when={phase().kind === "done"}>
            <p class="approve-sub" role="status">
              {(phase() as { approved?: boolean }).approved ? t("login.approveDone") : t("login.denyDone")}
            </p>
            <A class="btn btn--primary" href="/">
              {t("login.backHome")}
            </A>
          </Match>
          <Match when={phase().kind === "gone" || phase().kind === "missing"}>
            <p class="approve-sub" role="alert">
              {phase().kind === "gone" ? t("login.approveExpired") : t("login.approveMissing")}
            </p>
            <A class="btn" href="/">
              {t("login.backHome")}
            </A>
          </Match>
          <Match when={phase().kind === "error"}>
            <p class="approve-sub" role="alert">
              {t("login.approveLoadError")}
            </p>
            <button type="button" class="btn btn--primary" onClick={() => void load()}>
              {t("login.retry")}
            </button>
          </Match>
        </Switch>
      </div>
    </section>
  );
}
