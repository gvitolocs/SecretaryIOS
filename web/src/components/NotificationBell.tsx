import { useNavigate } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, on, Show, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import type { AppNotification, Category } from "../api/notifications";
import { locale, t } from "../i18n";
import { agentNotificationRoute } from "../lib/agentsRoute";
import { prefetchPushKey } from "../lib/push";
import {
  markAllRead,
  markRead,
  notifications,
  notificationsError,
  notificationsLoaded,
  pushBusy,
  pushStatus,
  refreshNotifications,
  safeRoute,
  titleOf,
  turnOnPush,
  unreadCount,
} from "../state/notifications";
import {
  BellIcon,
  CalendarIcon,
  ClockIcon,
  GearIcon,
  ListIcon,
  PlugIcon,
  PulseIcon,
  ShieldCheckIcon,
  SparkIcon,
} from "./Icons";

const ICONS: Record<Category, () => JSX.Element> = {
  lucina_message: SparkIcon,
  reminder: ClockIcon,
  calendar: CalendarIcon,
  device_confirm: ShieldCheckIcon,
  agent: PlugIcon,
  shopping: ListIcon,
  presence: PulseIcon,
  system: GearIcon,
  approval: ShieldCheckIcon,
};

const CHANNEL_NAMES: Record<string, string> = {
  telegram: "Telegram",
  discord: "Discord",
  alexa: "Alexa",
  web: "Web",
  ios: "iPhone",
};

/** "5 min ago", "3 h ago", or a date for older items. */
export function relativeTime(ts: number, now = Date.now(), loc = locale()): string {
  const diff = Math.max(0, now - ts);
  const min = Math.floor(diff / 60_000);
  if (min < 1) return t("notify.justNow");
  try {
    const rtf = new Intl.RelativeTimeFormat(loc, { numeric: "auto", style: "short" });
    if (min < 60) return rtf.format(-min, "minute");
    const h = Math.floor(min / 60);
    if (h < 24) return rtf.format(-h, "hour");
    return new Intl.DateTimeFormat(loc, { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" }).format(ts);
  } catch {
    return new Date(ts).toLocaleString();
  }
}

function sameDay(a: number, b: number): boolean {
  const x = new Date(a);
  const y = new Date(b);
  return x.getFullYear() === y.getFullYear() && x.getMonth() === y.getMonth() && x.getDate() === y.getDate();
}

function Item(props: { n: AppNotification; onOpen: (n: AppNotification) => void }) {
  const Icon = ICONS[props.n.category];
  const channel = () => (props.n.channel ? (CHANNEL_NAMES[props.n.channel] ?? props.n.channel) : null);
  return (
    <li>
      <button
        type="button"
        class="notif-item"
        classList={{ "notif-item--unread": !props.n.read }}
        onClick={() => props.onOpen(props.n)}
      >
        <span class={`notif-icon notif-icon--${props.n.category}`} aria-hidden="true">
          <Icon />
        </span>
        <span class="notif-text">
          <span class="notif-title">{titleOf(props.n)}</span>
          <Show when={props.n.body}>
            <span class="notif-body">{props.n.body}</span>
          </Show>
          <span class="notif-meta">
            <time dateTime={new Date(props.n.ts).toISOString()}>{relativeTime(props.n.ts)}</time>
            <Show when={channel()}>{(c) => <span> · {t("notify.via", { channel: c() })}</span>}</Show>
          </span>
        </span>
        <Show when={!props.n.read}>
          <span class="notif-dot" role="img" aria-label={t("notify.unread")} />
        </Show>
      </button>
    </li>
  );
}

/** Asks this browser to enable push (only from the button's click). */
export function PushCta() {
  const [error, setError] = createSignal<string | null>(null);
  return (
    <Show when={pushStatus() === "off" || pushStatus() === "needs-install" || pushStatus() === "denied"}>
      <div class="notif-cta" role="note">
        <span class="notif-cta-icon" aria-hidden="true">
          <BellIcon />
        </span>
        <div class="notif-cta-text">
          <p class="notif-cta-title">{t("notify.ctaTitle")}</p>
          <p class="muted small">
            {pushStatus() === "needs-install"
              ? t("notify.pushInstallIos")
              : pushStatus() === "denied"
                ? t("notify.pushDenied")
                : t("notify.ctaBody")}
          </p>
          <Show when={error()}>
            {(msg) => (
              <p class="form-error small" role="alert">
                {msg()}
              </p>
            )}
          </Show>
        </div>
        <Show when={pushStatus() === "off"}>
          <button
            type="button"
            class="btn btn--primary btn--small"
            disabled={pushBusy()}
            onClick={() => {
              setError(null);
              void turnOnPush().then((e) => setError(e));
            }}
          >
            {pushBusy() ? t("notify.pushWorking") : t("notify.pushEnable")}
          </button>
        </Show>
      </div>
    </Show>
  );
}

export function NotificationPanel(props: { open: boolean; onClose: () => void; onSettings: () => void }) {
  const navigate = useNavigate();
  let closeBtn: HTMLButtonElement | undefined;

  createEffect(
    on(
      () => props.open,
      (open) => {
        if (!open) return;
        void refreshNotifications();
        void prefetchPushKey();
        queueMicrotask(() => closeBtn?.focus());
      },
    ),
  );

  const groups = createMemo(() => {
    const now = Date.now();
    const list = notifications();
    return {
      today: list.filter((n) => sameDay(n.ts, now)),
      earlier: list.filter((n) => !sameDay(n.ts, now)),
    };
  });

  function open(n: AppNotification) {
    void markRead([n.id]);
    props.onClose();
    navigate(agentNotificationRoute(n) ?? safeRoute(n.route));
  }

  return (
    <Show when={props.open}>
      <div class="notif-overlay" onClick={(e) => e.target === e.currentTarget && props.onClose()}>
        <section
          class="notif-panel"
          role="dialog"
          aria-modal="true"
          aria-labelledby="notif-title"
          onKeyDown={(e) => {
            if (e.key === "Escape") props.onClose();
          }}
        >
          <header class="notif-head">
            <h2 id="notif-title" class="notif-heading">
              {t("notify.title")}
              <Show when={unreadCount() > 0}>
                <span class="notif-count">{unreadCount()}</span>
              </Show>
            </h2>
            <div class="notif-head-actions">
              <Show when={unreadCount() > 0}>
                <button type="button" class="btn-link notif-markall" onClick={() => void markAllRead()}>
                  {t("notify.markAllRead")}
                </button>
              </Show>
              <button
                type="button"
                class="icon-btn"
                aria-label={t("notify.settings")}
                title={t("notify.settings")}
                onClick={() => {
                  props.onClose();
                  props.onSettings();
                }}
              >
                <GearIcon />
              </button>
              <button type="button" class="icon-btn" ref={closeBtn} aria-label={t("common.close")} onClick={() => props.onClose()}>
                ×
              </button>
            </div>
          </header>
          <PushCta />
          <Show when={notificationsError() && notifications().length === 0}>
            <p class="form-error" role="alert">
              {t("notify.loadFailed")}
            </p>
          </Show>
          <Show
            when={notifications().length > 0}
            fallback={
              <Show when={notificationsLoaded() && !notificationsError()}>
                <div class="notif-empty">
                  <span class="notif-empty-icon" aria-hidden="true">
                    <BellIcon />
                  </span>
                  <p class="empty-title">{t("notify.empty")}</p>
                  <p class="empty-sub">{t("notify.emptySub")}</p>
                </div>
              </Show>
            }
          >
            <div class="notif-scroll">
              <Show when={groups().today.length > 0}>
                <h3 class="notif-group">{t("notify.today")}</h3>
                <ul class="notif-list">
                  <For each={groups().today}>{(n) => <Item n={n} onOpen={open} />}</For>
                </ul>
              </Show>
              <Show when={groups().earlier.length > 0}>
                <h3 class="notif-group">{t("notify.earlier")}</h3>
                <ul class="notif-list">
                  <For each={groups().earlier}>{(n) => <Item n={n} onOpen={open} />}</For>
                </ul>
              </Show>
            </div>
          </Show>
        </section>
      </div>
    </Show>
  );
}

/** The header bell with the unread badge; opens the panel. */
export function NotificationBell(props: { onSettings: () => void }) {
  const [open, setOpen] = createSignal(false);
  let button: HTMLButtonElement | undefined;
  const label = () => (unreadCount() > 0 ? t("notify.openUnread", { count: unreadCount() }) : t("notify.open"));
  return (
    <>
      <button
        type="button"
        class="icon-btn notif-bell"
        classList={{ "notif-bell--unread": unreadCount() > 0 }}
        ref={button}
        aria-label={label()}
        title={label()}
        aria-haspopup="dialog"
        aria-expanded={open() ? "true" : "false"}
        onClick={() => setOpen((v) => !v)}
      >
        <BellIcon />
        <Show when={unreadCount() > 0}>
          <span class="notif-badge" aria-hidden="true">
            {unreadCount() > 99 ? "99+" : unreadCount()}
          </span>
        </Show>
      </button>
      {/* In <body>: the header's backdrop-filter would trap a fixed overlay. */}
      <Portal>
        <NotificationPanel
          open={open()}
          onClose={() => {
            setOpen(false);
            button?.focus();
          }}
          onSettings={props.onSettings}
        />
      </Portal>
    </>
  );
}
