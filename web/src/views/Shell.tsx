import { A } from "@solidjs/router";
import { createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";
import { AccountDialog } from "../components/AccountDialog";
import { ChatIcon, DumbbellIcon, ListIcon, LogoutIcon, ScaleIcon, UserIcon } from "../components/Icons";
import { StatusDot } from "../components/StatusDot";
import { resumeJobPolling } from "../state/chat";
import { checkHealth, HEALTH_POLL_MS } from "../state/health";
import { lucinaNotice, markLucinaOnline } from "../state/lucina";
import { signOut } from "../state/session";

function NavLinks(props: { class: string }) {
  return (
    <nav class={props.class} aria-label="Sections">
      <A href="/" end class="tab" activeClass="tab--active">
        <ChatIcon />
        <span>Chat</span>
      </A>
      <A href="/shopping" class="tab" activeClass="tab--active">
        <ListIcon />
        <span>Shopping</span>
      </A>
      <A href="/gym" class="tab" activeClass="tab--active">
        <DumbbellIcon />
        <span>Gym</span>
      </A>
      <A href="/weight" class="tab" activeClass="tab--active">
        <ScaleIcon />
        <span>Weight</span>
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
        Skip to content
      </a>
      <header class="topbar">
        <div class="brand">
          <span class="brand-mark" aria-hidden="true">
            S
          </span>
          <span class="brand-name">Secretary</span>
        </div>
        <NavLinks class="tabs-top" />
        <div class="topbar-actions">
          <StatusDot />
          <button
            type="button"
            class="icon-btn"
            ref={accountButton}
            onClick={() => setAccountOpen(true)}
            aria-label="Account"
            title="Account"
          >
            <UserIcon />
          </button>
          <button
            type="button"
            class="icon-btn"
            onClick={() => void logout()}
            disabled={loggingOut()}
            aria-label="Log out"
            title="Log out"
          >
            <LogoutIcon />
          </button>
        </div>
      </header>
      <Show when={lucinaNotice()}>
        {(msg) => (
          <div class="banner" role="status">
            <span class="banner-text">{msg()}</span>
            <button type="button" class="banner-close" onClick={markLucinaOnline} aria-label="Dismiss">
              ×
            </button>
          </div>
        )}
      </Show>
      <main class="content" id="main" tabindex="-1">
        {props.children}
      </main>
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
