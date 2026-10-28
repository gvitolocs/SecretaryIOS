import { A, useNavigate } from "@solidjs/router";
import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { BackIcon } from "../components/Icons";
import { Button, Card, ErrorState, Page, PageGrid, PageHeader, useWide } from "../components/page";
import { t } from "../i18n";
import { approveCodeFrom, createQrReader, type QrReader } from "../lib/qrScan";

// /scan: approve a QR sign-in from this signed-in app (Account → Scan QR code, or Plugins).
// The phone's own camera app would open the link in a browser that is not signed in (a
// home-screen app keeps its own cookies), so the scan happens here, with this app's session.
// A found code opens /approve?c=…, which shows who is asking and asks Approve / Deny.
// The 6-digit code under the QR can be typed instead.

export const SCAN_EVERY_MS = 200;

type Camera = "starting" | "on" | "denied" | "unavailable";

export default function ScanQr() {
  const navigate = useNavigate();
  const { wide, track } = useWide();
  const [camera, setCamera] = createSignal<Camera>("starting");
  const [notOurs, setNotOurs] = createSignal(false);
  const [typed, setTyped] = createSignal("");
  const [typedError, setTypedError] = createSignal(false);
  let video: HTMLVideoElement | undefined;
  let stream: MediaStream | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let stopped = false;

  function stop() {
    stopped = true;
    if (timer !== undefined) clearTimeout(timer);
    stream?.getTracks().forEach((track) => track.stop());
    stream = undefined;
  }
  onCleanup(stop);

  function open(code: string) {
    stop();
    navigate(`/approve?c=${encodeURIComponent(code)}`);
  }

  async function loop(reader: QrReader) {
    if (stopped || !video) return;
    for (const text of await reader.read(video)) {
      const code = approveCodeFrom(text);
      if (code) return open(code);
      setNotOurs(true);
    }
    if (!stopped) timer = setTimeout(() => void loop(reader), SCAN_EVERY_MS);
  }

  onMount(async () => {
    if (!navigator.mediaDevices?.getUserMedia) {
      setCamera("unavailable");
      return;
    }
    try {
      stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: { ideal: "environment" } }, audio: false });
    } catch (e) {
      const name = (e as { name?: string })?.name;
      setCamera(name === "NotAllowedError" || name === "SecurityError" ? "denied" : "unavailable");
      return;
    }
    if (stopped) return stop();
    if (video) {
      video.srcObject = stream;
      void Promise.resolve(video.play?.()).catch(() => {});
    }
    setCamera("on");
    void loop(await createQrReader());
  });

  function onSubmit(e: SubmitEvent) {
    e.preventDefault();
    const code = approveCodeFrom(typed());
    if (code) open(code);
    else setTypedError(true);
  }

  const cameraMessage = () =>
    camera() === "starting" ? t("login.scanStarting") : camera() === "denied" ? t("login.scanDenied") : t("login.scanUnavailable");

  const scanner = () => (
    <>
      <p class="scan-sub">{t("login.scanSub")}</p>
      <Card class="scan-desk-card">
        <div class="scan-viewfinder" data-camera={camera()}>
          <video ref={video} class="scan-video" playsinline muted autoplay aria-hidden="true" />
          <span class="scan-frame" aria-hidden="true" />
          <Show when={camera() !== "on"}>
            <Show when={camera() === "starting"}>
              <p class="scan-note" role="status">
                {cameraMessage()}
              </p>
            </Show>
            <Show when={camera() !== "starting"}>
              <Show
                when={wide()}
                fallback={
                  <p class="scan-note" role="alert">
                    {cameraMessage()}
                  </p>
                }
              >
                <ErrorState message={cameraMessage()} retryLabel={t("common.tryAgain")} onRetry={() => window.location.reload()} />
              </Show>
            </Show>
          </Show>
        </div>
      </Card>
      <Show when={notOurs()}>
        <p class="scan-hint" role="status">
          {t("login.scanNotOurs")}
        </p>
      </Show>
      <form class="scan-code" onSubmit={onSubmit} novalidate>
        <label class="field-label" for="scan-code-input">
          {t("login.scanTypeLabel")}
        </label>
        <div class="scan-code-row">
          <input
            id="scan-code-input"
            class="text-input"
            inputmode="numeric"
            autocomplete="one-time-code"
            placeholder="123 456"
            maxlength={7}
            value={typed()}
            onInput={(e) => {
              setTyped(e.currentTarget.value);
              setTypedError(false);
            }}
            aria-invalid={typedError() ? "true" : "false"}
            aria-describedby={typedError() ? "scan-code-error" : undefined}
          />
          <Show when={wide()} fallback={
            <button type="submit" class="btn btn--primary">
              {t("login.scanContinue")}
            </button>
          }>
            <Button type="submit">{t("login.scanContinue")}</Button>
          </Show>
        </div>
        <Show when={typedError()}>
          <Show
            when={wide()}
            fallback={
              <p id="scan-code-error" class="form-error" role="alert">
                {t("login.scanTypeError")}
              </p>
            }
          >
            <ErrorState message={t("login.scanTypeError")} retryLabel={t("common.tryAgain")} onRetry={() => setTypedError(false)} />
          </Show>
        </Show>
      </form>
    </>
  );

  return (
    <section class="scan-page" classList={{ "scan-page--wide": wide() }} ref={track} aria-labelledby="scan-title">
      <Show
        when={wide()}
        fallback={
          <>
            <header class="scan-head">
              <A class="icon-btn" href="/plugins" aria-label={t("login.scanBack")}>
                <BackIcon />
              </A>
              <h1 id="scan-title" class="scan-title">
                {t("login.scanTitle")}
              </h1>
            </header>
            {scanner()}
          </>
        }
      >
        <Page class="scan-desk">
          <PageHeader title={t("login.scanTitle")} />
          <PageGrid layout="single">{scanner()}</PageGrid>
        </Page>
      </Show>
    </section>
  );
}
