import { useNavigate, useParams } from "@solidjs/router";
import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import { Card, EmptyState, ErrorState, ListRow, LoadingState, Page, PageGrid, PageHeader, PrimaryAction, RowList, useWide } from "../components/page";
import { api } from "../api/secretary";
import type { TimelineItem } from "../api/timeline";
import { relativeTime } from "../lib/relativeTime";
import { t } from "../i18n";
import { conversationKey, getState, readItems, setState, type Scope } from "../lib/chatdb";
import { olderScope, syncScope, type Conversation } from "../lib/chatSync";
import { chatDevice, fromItem, onTimelineConnected, onTimelineItem, setReplyTarget } from "../state/chat";
import { ToolSteps } from "../components/ToolSteps";
import { chatRows, isToolRun } from "../lib/toolSteps";
import { MessageItem } from "./Chat";
import "../styles/chats.css";
import "../styles/tool-steps.css";

// All chats (Plugins → All chats): every conversation Lucina has, across devices (this browser,
// the iPhone, the Mac), MCP agents (ChatGPT, Claude, Grok Bot, Cursor, Codex) and bots
// (Telegram, Discord, Alexa). /chats lists them (GET /v1/timeline/conversations, cached in
// IndexedDB for an instant list); /chats/:key shows one read-only from the local store, then
// syncs it with `conversation=<key>`. A device conversation can be continued here: the main
// chat opens replying to its last message. This device's own conversation is the main chat.

/** The route's `:key`, decoded (`device%3Aabc` → `device:abc`); undefined on the list. */
function routeKey(raw: string | undefined): string | undefined {
  if (!raw) return undefined;
  try {
    return decodeURIComponent(raw);
  } catch {
    return raw;
  }
}

const kindLabel = (c: Conversation) =>
  t(c.kind === "device" || c.kind === "legacy" ? "chats.device" : c.kind === "agent" ? "chats.agent" : "chats.bot");

export default function ChatsView() {
  const { wide, track } = useWide();
  const params = useParams();
  const navigate = useNavigate();
  const [conversations, setConversations] = createSignal<Conversation[]>([]);
  const [items, setItems] = createSignal<TimelineItem[]>([]);
  const [before, setBefore] = createSignal<string | null>(null);
  const [error, setError] = createSignal(false);
  const [loading, setLoading] = createSignal(false);
  const [refresh, setRefresh] = createSignal(0);
  const refreshNow = () => setRefresh((n) => n + 1);

  // Sync again when the stream reconnects or the tab comes back.
  onCleanup(onTimelineConnected(refreshNow));
  const visible = () => {
    if (document.visibilityState === "visible") refreshNow();
  };
  document.addEventListener("visibilitychange", visible);
  onCleanup(() => document.removeEventListener("visibilitychange", visible));

  const key = () => routeKey(params.key);
  const selected = () => conversations().find((c) => c.key === key());
  const own = (c: Conversation) => !!chatDevice() && c.key === `device:${chatDevice()}`;
  const merge = (received: TimelineItem[], reset = false) =>
    setItems((old) => {
      const map = new Map((reset ? [] : old).map((i) => [i.id, i]));
      for (const i of received) map.set(i.id, i);
      return [...map.values()].sort((a, b) => a.ts - b.ts || a.id.localeCompare(b.id, undefined, { numeric: true }));
    });

  createEffect(() => {
    refresh();
    const current = key();
    let active = true;
    onCleanup(() => {
      active = false;
    });
    setItems([]);
    setBefore(null);
    setError(false);
    setLoading(true);

    async function load() {
      const cachedList = await getState<Conversation[]>("conversations");
      if (!active) return;
      if (cachedList) setConversations(cachedList);
      void api
        .getConversations()
        .then(async (result) => {
          if (!active) return;
          setConversations(result.items);
          await setState("conversations", result.items);
        })
        .catch(() => {
          if (active && !current) setError(true);
        })
        .finally(() => {
          if (active && !current) setLoading(false);
        });
      if (!current) return;

      // One conversation: the local copy first, then whatever arrived since its cursor.
      const scope: Scope = `conversation:${current}`;
      const cached = await readItems(scope);
      if (!active) return;
      merge(cached, true);
      if (cached.length) setBefore(cached[cached.length - 1]!.id);
      try {
        const page = await syncScope(scope, undefined, () => active);
        if (!active) return;
        merge(page.items, page.reset);
        if (!cached.length || page.reset) setBefore(page.nextBefore);
      } catch {
        if (active) setError(true);
      } finally {
        if (active) setLoading(false);
      }
    }
    void load();
  });

  onCleanup(
    onTimelineItem((item) => {
      if (conversationKey(item) === key()) merge([item]);
    }),
  );

  /** Older messages: the local store first, then the server. */
  async function older() {
    const current = key();
    if (loading() || !before() || !current) return;
    const scope: Scope = `conversation:${current}`;
    setLoading(true);
    setError(false);
    try {
      const cached = await readItems(scope, undefined, items()[0]?.id ?? before());
      const page = cached.length
        ? { items: cached, nextBefore: cached[cached.length - 1]!.id }
        : await olderScope(scope, before()!);
      if (key() !== current) return;
      merge(page.items);
      setBefore(page.nextBefore === before() ? null : page.nextBefore);
    } catch {
      setError(true);
    } finally {
      setLoading(false);
    }
  }

  const canContinue = () => /^(device|legacy):/.test(key() ?? "") && key() !== `device:${chatDevice()}`;
  function continueHere() {
    const last = items().at(-1);
    if (!last) return;
    setReplyTarget({ id: last.id, text: last.text });
    navigate("/");
  }

  const conversationBody = () => (          <>
            <Show when={!wide()}>
            <button class="btn" onClick={() => navigate("/chats")}>
              {t("chats.title")}
            </button>
            <Show when={canContinue()} fallback={<p>{t("chats.lives", { place: selected()?.title ?? key() ?? "" })}</p>}>
              <button class="btn btn--primary" disabled={!items().length} onClick={continueHere}>
                {t("chats.continue")}
              </button>
            </Show>
            </Show>
            <Show when={wide() && !canContinue()}><p class="pg-muted">{t("chats.lives", { place: selected()?.title ?? key() ?? "" })}</p></Show>
            <div
              class={wide() ? "chats-desk-messages" : "conversation-messages"}
              onScroll={(e) => {
                if (e.currentTarget.scrollTop < 80) void older();
              }}
            >
              <Show when={before()}>
                <button class={wide() ? "pg-btn" : "btn"} disabled={loading()} onClick={() => void older()}>
                  {t("chat.loadEarlier")}
                </button>
              </Show>
              <fieldset disabled class="readonly-messages" aria-label={t("chat.conversation")}>
                <For each={chatRows(items().map(fromItem))}>
                  {(row) => (isToolRun(row) ? <ToolSteps steps={row.steps} /> : <MessageItem readOnly message={row} />)}
                </For>
              </fieldset>
            </div>
            <Show when={wide() && !loading() && !items().length && !error()}><EmptyState text={t("chats.emptyConversation")} /></Show>
          </>);
  const list = () => <Card list class="chats-desk-list" label={t("chats.title")}>
    <Show when={conversations().length} fallback={<Show when={!loading() && !error()}><EmptyState text={t("chats.empty")} /></Show>}>
      <RowList label={t("chats.title")}><For each={conversations()}>{c => <ListRow
        leading={<span class="chats-desk-avatar" aria-hidden="true">{c.title.trim()[0] ?? "?"}</span>}
        title={<a class="pg-row-name pg-row-link" href={own(c) ? "/" : `/chats/${encodeURIComponent(c.key)}`} aria-current={c.key === key() ? "page" : undefined}>{c.title}</a>}
        badge={kindLabel(c)} sub={<>{own(c) ? `${t("chats.thisDevice")} · ` : ""}{c.lastText}</>}
        meta={<time>{relativeTime(c.lastTs)}</time>}
        trailing={<span class="pg-badge" aria-label={t("chats.messageCount", { count: c.count })}>{c.count}</span>}
      />}</For></RowList>
    </Show>
  </Card>;
  const status = () => <>
    <Show when={error()}><ErrorState message={t("chat.loadFailed")} retryLabel={t("common.tryAgain")} onRetry={refreshNow} /></Show>
    <Show when={loading()}><LoadingState label={t("common.loading")} /></Show>
  </>;
  return (
    <div ref={track}><Show when={wide()} fallback={<section class="chats-page">
      <h1 id="chats-title">{selected()?.title ?? t("chats.title")}</h1>
      <Show when={error()}>
        <p role="alert">
          {t("chat.loadFailed")}{" "}
          <button class="btn" onClick={() => window.location.reload()}>
            {t("common.tryAgain")}
          </button>
        </p>
      </Show>
      <Show
        when={!key()}
        fallback={
          conversationBody()
        }
      >
        <div class="conversation-list">
          <For each={conversations()}>
            {(c) => (
              <button class="conversation-row" onClick={() => navigate(own(c) ? "/" : `/chats/${encodeURIComponent(c.key)}`)}>
                <strong>{c.title}</strong>
                <span class="conversation-kind">{kindLabel(c)}</span>
                <Show when={own(c)}>
                  <span>{t("chats.thisDevice")}</span>
                </Show>
                <p>{c.lastText}</p>
                <time>{relativeTime(c.lastTs)}</time>
                <span>{c.count}</span>
              </button>
            )}
          </For>
        </div>
        <Show when={!loading() && !conversations().length}>
          <p>{t("chats.empty")}</p>
        </Show>
      </Show>
      <Show when={loading()}>
        <p role="status">{t("common.loading")}</p>
      </Show>
    </section>}><Page>
      <PageHeader title={t("chats.title")} context={t("chats.count", { count: conversations().length })}
        action={<Show when={key() && canContinue()}><PrimaryAction disabled={!items().length} onClick={continueHere}>{t("chats.continue")}</PrimaryAction></Show>} />
      <Show when={key()} fallback={<PageGrid layout="single">{status()}{list()}</PageGrid>}>
        <PageGrid layout="split" rail={list()} railLabel={t("chats.title")}>
          <a class="pg-link pg-back" href="/chats">{t("chats.title")}</a>
          <h2 class="chats-desk-title">{selected()?.title ?? key()}</h2>
          {status()}
          <Card label={t("chat.conversation")}>{conversationBody()}</Card>
        </PageGrid>
      </Show>
    </Page></Show></div>
  );
}
