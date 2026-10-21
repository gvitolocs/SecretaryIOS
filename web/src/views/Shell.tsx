import { A } from "@solidjs/router";
import { createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";
import { AccountDialog } from "../components/AccountDialog";
import { AvatarIcon, ChatIcon, DumbbellIcon, ListIcon, LogoutIcon, ScaleIcon, UserIcon } from "../components/Icons";
import { LucinaDock } from "../components/LucinaDock";
import { StatusDot } from "../components/StatusDot";
import { t } from "../i18n";
import { resumeJobPolling } from "../state/chat";
import { checkHealth, HEALTH_POLL_MS } from "../state/health";
import { lucinaNotice, markLucinaOnline } from "../state/lucina";
import { signOut } from "../state/session";
import { setStageHidden, stageHidden } from "../state/stage";

function NavLinks(props: { class: string }) {
  return (
    <nav class={props.class} aria-label={t("shell.sections")}>
      <A href="/" end class="tab" activeClass="tab--active">
        <ChatIcon />
        <span>{t("shell.navChat")}</span>
      </A>
      <A href="/shopping" class="tab" activeClass="tab--active">
        <ListIcon />
        <span>{t("shell.navShopping")}</span>
      </A>
      <A href="/gym" class="tab" activeClass="tab--active">
        <DumbbellIcon />
        <span>{t("shell.navGym")}</span>
      </A>
      <A href="/weight" class="tab" activeClass="tab--active">
        <ScaleIcon />
        <span>{t("shell.navWeight")}</span>
      </A>
    </nav>
  );
}

export default function Shell(props: { children?: JSX.Element }) {
  const [loggingOut, setLoggingOut] = createSignal(false);
  const [accountOpen, setAccountOpen] = createSignal(false);
  let accountButton: HTMLButtonElement | undefined;

  onMount(() => {
    void checkHealth();
    resumeJobPolling();
    const timer = setInterval(() => void checkHealth(), HEALTH_POLL_MS);
    const onVisible = () => {
      if (document.visibilityState === "visible") void checkHealth();
    };
    document.addEventListener("visibilitychange", onVisible);
    onCleanup(() => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    });
  });

  async function logout() {
    if (loggingOut()) return;
    setLoggingOut(true);
    try {
      await signOut();
    } finally {
      setLoggingOut(false);
    }
  }

  return (
    <div class="shell">
      <a class="skip-link" href="#main">
        {t("shell.skipToContent")}
      </a>
      <header class="topbar">
        <div class="brand">
          <span class="brand-mark" aria-hidden="true">
            <img src="/icon-192.png" alt="" width="32" height="32" />
          </span>
          <span class="brand-name">Secretary</span>
        </div>
        <NavLinks class="tabs-top" />
        <div class="topbar-actions">
          <StatusDot />
          <Show when={stageHidden()}>
            <button
              type="button"
              class="icon-btn"
              onClick={() => setStageHidden(false)}
              aria-label={t("shell.lucinaShow")}
              title={t("shell.lucinaShow")}
            >
              <AvatarIcon />
            </button>
          </Show>
          <button
            type="button"
            class="icon-btn"
            ref={accountButton}
            onClick={() => setAccountOpen(true)}
            aria-label={t("shell.account")}
            title={t("shell.account")}
          >
            <UserIcon />
          </button>
          <button
            type="button"
            class="icon-btn"
            onClick={() => void logout()}
            disabled={loggingOut()}
            aria-label={t("shell.logout")}
            title={t("shell.logout")}
          >
            <LogoutIcon />
          </button>
        </div>
      </header>
      <Show when={lucinaNotice()}>
        {(msg) => (
          <div class="banner" role="status">
            <span class="banner-text">{msg()}</span>
            <button type="button" class="banner-close" onClick={markLucinaOnline} aria-label={t("common.dismiss")}>
              ×
            </button>
          </div>
        )}
      </Show>
      {/* Page column + Lucina. The dock lives here so it survives navigation. */}
      <div class="shell-body">
        <main class="content" id="main" tabindex="-1">
          {props.children}
        </main>
        <LucinaDock />
      </div>
      <NavLinks class="tabbar" />
      <AccountDialog
        open={accountOpen()}
        onClose={() => {
          setAccountOpen(false);
          accountButton?.focus();
        }}
      />
    </div>
  );
}
