import { useNavigate, useParams } from "@solidjs/router";
import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import { api } from "../api/secretary";
import { parseTs, type TimelineItem } from "../api/timeline";
import { locale, t } from "../i18n";
import { conversationKey, getState, readItems, setState, type Scope } from "../lib/chatdb";
import { olderScope, syncScope, type Conversation } from "../lib/chatSync";
import { chatDevice, fromItem, onTimelineConnected, onTimelineItem, setReplyTarget } from "../state/chat";
import { MessageItem } from "./Chat";
import "../styles/chats.css";

// All chats (Plugins → All chats): every conversation Lucina has, across devices (this browser,
// the iPhone, the Mac), MCP agents (ChatGPT, Claude, Grok Bot, Cursor, Codex) and bots
// (Telegram, Discord, Alexa). /chats lists them (GET /v1/timeline/conversations, cached in
// IndexedDB for an instant list); /chats/:key shows one read-only from the local store, then
// syncs it with `conversation=<key>`. A device conversation can be continued here: the main
// chat opens replying to its last message. This device's own conversation is the main chat.

/** "3 minutes ago", "yesterday": `ts` relative to now, in the app's locale. */
export function relativeTime(ts: number | string): string {
  const seconds = Math.round(((parseTs(ts) ?? Date.now()) - Date.now()) / 1000);
  const abs = Math.abs(seconds);
  const [unit, size]: [Intl.RelativeTimeFormatUnit, number] =
    abs >= 86_400 ? ["day", 86_400] : abs >= 3_600 ? ["hour", 3_600] : abs >= 60 ? ["minute", 60] : ["second", 1];
  return new Intl.RelativeTimeFormat(locale(), { numeric: "auto" }).format(Math.round(seconds / size), unit);
}

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

  return (
    <section class="chats-page">
      <h1>{selected()?.title ?? t("chats.title")}</h1>
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
          <>
            <button class="btn" onClick={() => navigate("/chats")}>
              {t("chats.title")}
            </button>
            <Show when={canContinue()} fallback={<p>{t("chats.lives", { place: selected()?.title ?? key() ?? "" })}</p>}>
              <button class="btn btn--primary" disabled={!items().length} onClick={continueHere}>
                {t("chats.continue")}
              </button>
            </Show>
            <div
              class="conversation-messages"
              onScroll={(e) => {
                if (e.currentTarget.scrollTop < 80) void older();
              }}
            >
              <Show when={before()}>
                <button class="btn" disabled={loading()} onClick={() => void older()}>
                  {t("chat.loadEarlier")}
                </button>
              </Show>
              <fieldset disabled class="readonly-messages" aria-label={t("chat.conversation")}>
                <For each={items()}>{(item) => <MessageItem readOnly message={fromItem(item)} />}</For>
              </fieldset>
            </div>
          </>
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
    </section>
  );
}
