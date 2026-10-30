import { useNavigate } from "@solidjs/router";
import { decisionSummary } from "../lib/decisionText";
import { createMemo, For, onMount, Show } from "solid-js";
import type { AppNotification } from "../api/notifications";
import { t } from "../i18n";
import { agentNotificationRoute } from "../lib/agentsRoute";
import { prefetchPushKey } from "../lib/push";
import {
  markAllRead,
  markRead,
  notifications,
  notificationsError,
  notificationsLoaded,
  refreshNotifications,
  safeRoute,
  titleOf,
  unreadCount,
} from "../state/notifications";
import { channelName, groupByDay, ICONS, PushCta, relativeTime } from "./NotificationBell";
import { GearIcon } from "./Icons";
import { Button, Card, EmptyState, ErrorState, ListRow, LoadingState, Page, PageHeader, PageGrid, RowList } from "./page";

/** The /notifications route on a desktop: the bell panel's content as a page-system page (the panel itself is unchanged). */
export function NotificationsPage() {
  const navigate = useNavigate();
  onMount(() => {
    void refreshNotifications();
    void prefetchPushKey();
  });
  const groups = createMemo(() => groupByDay(notifications()));

  function open(n: AppNotification) {
    void markRead([n.id]);
    navigate(agentNotificationRoute(n) ?? safeRoute(n.route));
  }

  function Group(p: { title: string; items: AppNotification[] }) {
    return (
      <Show when={p.items.length > 0}>
        <Card list title={p.title}>
          <RowList>
            <For each={p.items}>
              {(n) => {
                const Icon = ICONS[n.category];
                const channel = () => channelName(n.channel);
                return (
                  <ListRow
                    leading={
                      <span class={`notif-icon notif-icon--${n.category}`} aria-hidden="true">
                        <Icon />
                      </span>
                    }
                    title={
                      <button type="button" class="pg-row-name pg-row-link" onClick={() => open(n)}>
                        {titleOf(n)}
                      </button>
                    }
                    sub={n.body ? decisionSummary(n.body) : n.body}
                    meta={
                      <time dateTime={new Date(n.ts).toISOString()}>
                        {relativeTime(n.ts)}
                        {channel() ? ` · ${t("notify.via", { channel: channel()! })}` : ""}
                      </time>
                    }
                    trailing={
                      <Show when={!n.read}>
                        <span class="notif-dot" role="img" aria-label={t("notify.unread")} />
                      </Show>
                    }
                  />
                );
              }}
            </For>
          </RowList>
        </Card>
      </Show>
    );
  }

  return (
    <Page>
      <PageHeader
        title={t("notify.title")}
        context={unreadCount() > 0 ? t("notify.unreadContext", { count: unreadCount() }) : t("notify.allReadContext")}
        action={
          <>
            <Show when={unreadCount() > 0}>
              <Button onClick={() => void markAllRead()}>{t("notify.markAllRead")}</Button>
            </Show>
            <a class="pg-icon-btn" href="/account?section=notifications" aria-label={t("notify.settings")} title={t("notify.settings")}>
              <GearIcon />
            </a>
          </>
        }
      />
      <PageGrid layout="single">
        <PushCta desktop />
        <Show when={notifications().length === 0}>
          <Show
            when={!(notificationsError() && notificationsLoaded())}
            fallback={<ErrorState message={t("notify.loadFailed")} retryLabel={t("common.tryAgain")} onRetry={() => void refreshNotifications()} />}
          >
            <Show when={notificationsLoaded()} fallback={<LoadingState label={t("common.loading")} />}>
              <Card>
                <EmptyState title={t("notify.empty")} text={t("notify.emptySub")} />
              </Card>
            </Show>
          </Show>
        </Show>
        <Group title={t("notify.today")} items={groups().today} />
        <Group title={t("notify.earlier")} items={groups().earlier} />
      </PageGrid>
    </Page>
  );
}
