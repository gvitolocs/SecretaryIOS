import { A, useLocation, useNavigate } from "@solidjs/router";
import { createEffect, createSignal, lazy, on, onCleanup, onMount, Show, Suspense, type JSX } from "solid-js";
import { ApprovalPopup } from "../components/ApprovalPopup";
import { AccountDialog } from "../components/AccountDialog";
import { SecretSheet } from "../components/SecretSheet";
import { AvatarIcon, ChatIcon, DumbbellIcon, GridIcon, ListIcon, LogoutIcon, UserIcon } from "../components/Icons";
import { LucinaDock } from "../components/LucinaDock";
import { NotificationBell } from "../components/NotificationBell";
import { StatusDot } from "../components/StatusDot";
import { listShares } from "../lib/share";
import { t } from "../i18n";
import { showWebChrome } from "../lib/desktopShell";
import "../styles/embedded.css";
import { connectTimeline, resumeJobPolling } from "../state/chat";
import { checkHealth, HEALTH_POLL_MS } from "../state/health";
import { lucinaNotice, markLucinaOnline } from "../state/lucina";
import { startNotifications } from "../state/notifications";
import { signOut } from "../state/session";
import { setStageHidden, stageHidden } from "../state/stage";
import { setAvatarPath, startAvatarSync, wardrobeOpen } from "../state/wardrobe";
import "../styles/character-shell.css";

// The Wardrobe panel is only fetched the first time it opens.
const Wardrobe = lazy(() => import("../components/Wardrobe"));

// The owner's four sections (2026-10-24), the same as the iPhone tabs: Chat, Shopping,
// Training + Weight, Plugins. Everything else is a plugin card on /plugins.
const TRAINING_PATHS = ["/training", "/gym", "/weight"];
const PLUGIN_PATHS = ["/chats", "/plugins", "/notes", "/keep", "/tasks", "/links", "/models", "/memos", "/desk", "/library", "/media", "/drive", "/server-health", "/computer", "/nclient"];

function NavLinks(props: { class: string }) {
  const location = useLocation();
  const under = (paths: string[]) => paths.some((p) => location.pathname === p || location.pathname.startsWith(`${p}/`));
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
      <A href="/training" class="tab" classList={{ "tab--active": under(TRAINING_PATHS) }} aria-current={under(TRAINING_PATHS) ? "page" : undefined}>
        <DumbbellIcon />
        <span>{t("shell.navTraining")}</span>
      </A>
      <A href="/plugins" class="tab" classList={{ "tab--active": under(PLUGIN_PATHS) }} aria-current={under(PLUGIN_PATHS) ? "page" : undefined}>
        <GridIcon />
        <span>{t("shell.navPlugins")}</span>
      </A>
    </nav>
  );
}

function reducedMotion(): boolean {
  try {
    return typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch {
    return false;
  }
}

/** The page fades up into place when the section changes (not on first paint).
 * Moving between notes stays inside one page, so it does not count. */
function useRouteTransition(target: () => HTMLElement | undefined) {
  const location = useLocation();
  createEffect(
    on(
      () => (location.pathname.startsWith("/notes") ? "/notes" : location.pathname.startsWith("/keep") ? "/keep" : location.pathname.startsWith("/tasks") ? "/tasks" : location.pathname),
      () => {
        const el = target();
        if (!el || reducedMotion() || typeof el.animate !== "function") return;
        el.animate(
          [
            { opacity: 0, transform: "translateY(10px) scale(0.995)", filter: "blur(6px)" },
            { opacity: 1, transform: "none", filter: "blur(0)" },
          ],
          { duration: 380, easing: "cubic-bezier(0.16, 1, 0.3, 1)" },
        );
      },
      { defer: true },
    ),
  );
}

export default function Shell(props: { children?: JSX.Element }) {
  const [loggingOut, setLoggingOut] = createSignal(false);
  const [accountOpen, setAccountOpen] = createSignal(false);
  const [accountSection, setAccountSection] = createSignal<"notifications" | null>(null);
  let accountButton: HTMLButtonElement | undefined;
  let main: HTMLElement | undefined;
  useRouteTransition(() => main);
  const location = useLocation();
  // Her outfit follows the section (Gym, Shopping, elsewhere).
  createEffect(() => setAvatarPath(location.pathname));
  const [wardrobeLoaded, setWardrobeLoaded] = createSignal(false);
  createEffect(() => wardrobeOpen() && setWardrobeLoaded(true));
  const navigate = useNavigate();

  onMount(() => {
    if (typeof indexedDB !== "undefined") void listShares().then((entries) => {
      if (entries.length && location.pathname !== "/") navigate("/" + location.search);
    }).catch(() => undefined);
    onCleanup(startNotifications((route) => navigate(route)));
    void checkHealth();
    resumeJobPolling();
    // The timeline stream stays open on every page, so Lucina's outfit changes arrive live.
    onCleanup(connectTimeline());
    onCleanup(startAvatarSync());
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

  // Inside Secretary Code the desktop rail replaces the top bar and the tab bar, and names the pages (styles/embedded.css).
  const chrome = showWebChrome();
  if (!chrome) document.documentElement.dataset.shell = "desktop";

  return (
    <div class="shell shell--character" classList={{ "shell--desktop": !chrome }}>
      <a class="skip-link" href="#main">
        {t("shell.skipToContent")}
      </a>
      <Show when={chrome}>
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
            <NotificationBell
              onSettings={() => {
                setAccountSection("notifications");
                setAccountOpen(true);
              }}
            />
            <button
              type="button"
              class="icon-btn"
              ref={accountButton}
              onClick={() => {
                setAccountSection(null);
                setAccountOpen(true);
              }}
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
      </Show>
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
        <main class="content" id="main" tabindex="-1" ref={main}>
          <Suspense
            fallback={
              <div class="route-loading" role="status" aria-label={t("common.loading")}>
                <span class="holo-loader" aria-hidden="true" />
              </div>
            }
          >
            {props.children}
          </Suspense>
        </main>
        <LucinaDock />
      </div>
      <Show when={chrome}>
        <NavLinks class="tabbar" />
      </Show>
      <Show when={wardrobeLoaded()}>
        <Suspense>
          <Wardrobe />
        </Suspense>
      </Show>
      <SecretSheet />
      {/* Outside the chrome: Secretary Code has no top or tab bar, and the pop-up must still show. */}
      <ApprovalPopup />
      <AccountDialog
        open={accountOpen()}
        section={accountSection()}
        onClose={() => {
          setAccountOpen(false);
          accountButton?.focus();
        }}
      />
    </div>
  );
}
