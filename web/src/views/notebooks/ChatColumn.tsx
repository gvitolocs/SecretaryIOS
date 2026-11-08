import { createEffect, createSignal, For, on, Show } from "solid-js";
import { toApiError } from "../../api/client";
import { chunkOrdinal, notebooksApi, refSourceId, type Chat, type ChatModel, type Citation, type Message } from "../../api/notebooks";
import { EditIcon, PlusIcon, SendIcon, TrashIcon } from "../../components/Icons";
import { Markdown } from "../../components/Markdown";
import { Button, Card, EmptyState, LoadingState } from "../../components/page";
import { t } from "../../i18n";
import { citationRefs, parseMarkdown } from "../../lib/markdown";

function modelLabel(model: string | null | undefined): string {
  if (!model) return "";
  return /qwen/i.test(model) ? t("notebooks.modelQwen") : t("notebooks.modelDeepseek");
}

/** The citations a reply uses, in the order its chips are numbered. */
export function citedIn(m: Message): Citation[] {
  const refs = citationRefs(parseMarkdown(m.text));
  const have = m.citations ?? [];
  return refs.flatMap((ref) => have.filter((c) => c.ref === ref));
}

/**
 * The chat of a notebook: sessions picker, messages with numbered citation chips, a composer
 * with the model choice. Replies stream in when the server streams.
 */
export function ChatColumn(props: {
  notebookId: string;
  chats: Chat[];
  /** Whether any source is ready, so there is something to ask about. */
  hasSources: boolean;
  /** A citation was clicked: open the source at the passage (ordinal null: at the top). */
  onCite: (sourceId: string, ordinal: number | null) => void;
  onNoteSaved: () => void;
}) {
  const byRecent = (list: Chat[]) => [...list].sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at));
  const [chats, setChats] = createSignal<Chat[]>(byRecent(props.chats));
  const [activeId, setActiveId] = createSignal<string | null>(chats()[0]?.id ?? null);
  const [messages, setMessages] = createSignal<Message[]>([]);
  const [loading, setLoading] = createSignal(false);
  const [pending, setPending] = createSignal<{ user: string; reply: string } | null>(null);
  const [draft, setDraft] = createSignal("");
  const [model, setModel] = createSignal<ChatModel>("deepseek");
  const [error, setError] = createSignal<string | null>(null);
  const [saved, setSaved] = createSignal<Record<string, string>>({});
  let log: HTMLOListElement | undefined;
  let generation = 0;

  const sending = () => pending() !== null;

  function toBottom() {
    requestAnimationFrame(() => {
      if (log) log.scrollTop = log.scrollHeight;
    });
  }

  createEffect(
    on(activeId, async (id) => {
      const mine = ++generation;
      if (sending()) return;
      setMessages([]);
      setSaved({});
      if (!id) return;
      setLoading(true);
      try {
        const chat = await notebooksApi.chat(id);
        if (mine === generation) {
          setMessages(chat.messages);
          toBottom();
        }
      } catch (e) {
        if (mine === generation) setError(toApiError(e).message);
      } finally {
        if (mine === generation) setLoading(false);
      }
    }),
  );

  async function refreshChats() {
    try {
      setChats(byRecent(await notebooksApi.chats(props.notebookId)));
    } catch {
      // the list is only a convenience; the next action refreshes it
    }
  }

  async function send() {
    const text = draft().trim();
    if (!text || sending()) return;
    setError(null);
    setDraft("");
    setPending({ user: text, reply: "" });
    toBottom();
    try {
      let id = activeId();
      if (!id) {
        const chat = await notebooksApi.createChat(props.notebookId, text.slice(0, 60));
        generation++; // the new chat is empty: do not let the picker's load wipe the exchange
        setChats([chat, ...chats()]);
        setActiveId(chat.id);
        id = chat.id;
      }
      const exchange = await notebooksApi.sendMessage(id, { text, model: model() }, (piece) => {
        setPending((p) => (p ? { ...p, reply: p.reply + piece } : p));
        toBottom();
      });
      setMessages([...messages(), exchange.user, exchange.reply]);
      void refreshChats();
    } catch (e) {
      setError(toApiError(e).message);
      setDraft(text);
    } finally {
      setPending(null);
      toBottom();
    }
  }

  async function rename() {
    const id = activeId();
    const current = chats().find((c) => c.id === id);
    if (!id || !current) return;
    const title = prompt(t("notebooks.renameChatPrompt"), current.title)?.trim();
    if (!title) return;
    try {
      const next = await notebooksApi.renameChat(id, title);
      setChats(chats().map((c) => (c.id === id ? { ...c, title: next.title } : c)));
    } catch (e) {
      setError(toApiError(e).message);
    }
  }

  async function remove() {
    const id = activeId();
    const current = chats().find((c) => c.id === id);
    if (!id || !current || !confirm(t("notebooks.deleteChatConfirm", { title: current.title }))) return;
    try {
      await notebooksApi.removeChat(id);
      const rest = chats().filter((c) => c.id !== id);
      setChats(rest);
      setActiveId(rest[0]?.id ?? null);
    } catch (e) {
      setError(toApiError(e).message);
    }
  }

  async function saveToNote(m: Message) {
    const id = activeId();
    if (!id) return;
    setError(null);
    try {
      const link = await notebooksApi.messageToNote(id, m.id);
      setSaved({ ...saved(), [m.id]: link.slug });
      props.onNoteSaved();
    } catch (e) {
      setError(toApiError(e).message);
    }
  }

  function openCitation(m: Message, ref: string) {
    const c = (m.citations ?? []).find((x) => x.ref === ref);
    const sourceId = c?.source_id ?? refSourceId(ref);
    if (!sourceId) return;
    props.onCite(sourceId, ref.startsWith("source:") ? (c?.chunk ?? chunkOrdinal(ref)) : null);
  }

  return (
    <Card class="nb-card nb-chat" title={t("notebooks.chat")}>
      <div class="nb-chat-bar">
        <select
          class="pg-input"
          aria-label={t("notebooks.chatSessions")}
          onChange={(e) => setActiveId(e.currentTarget.value || null)}
          disabled={sending()}
        >
          <Show when={!activeId()}>
            <option value="" selected>
              {t("notebooks.newChat")}
            </option>
          </Show>
          <For each={chats()}>
            {(c) => (
              <option value={c.id} selected={c.id === activeId()}>
                {c.title}
              </option>
            )}
          </For>
        </select>
        <button type="button" class="pg-icon-btn" aria-label={t("notebooks.newChat")} title={t("notebooks.newChat")} disabled={sending()} onClick={() => setActiveId(null)}>
          <PlusIcon />
        </button>
        <button type="button" class="pg-icon-btn" aria-label={t("notebooks.renameChat")} title={t("notebooks.renameChat")} disabled={!activeId() || sending()} onClick={() => void rename()}>
          <EditIcon />
        </button>
        <button type="button" class="pg-icon-btn" aria-label={t("notebooks.deleteChat")} title={t("notebooks.deleteChat")} disabled={!activeId() || sending()} onClick={() => void remove()}>
          <TrashIcon />
        </button>
      </div>

      <ol class="nb-log" ref={log} role="log" aria-label={t("notebooks.chat")} aria-live="polite">
        <Show when={loading()}>
          <li><LoadingState label={t("notebooks.loading")} /></li>
        </Show>
        <Show when={!loading() && !messages().length && !pending()}>
          <li class="nb-log-empty">
            <EmptyState text={props.hasSources ? t("notebooks.chatEmpty") : t("notebooks.chatNoSources")} />
          </li>
        </Show>
        <For each={messages()}>
          {(m) => (
            <li class="nb-msg" data-role={m.role}>
              <Show when={m.role === "user"} fallback={
                <>
                  <Markdown
                    text={m.text}
                    class="nb-msg-text"
                    citations={m.citations}
                    onCite={(ref) => openCitation(m, ref)}
                  />
                  <Show when={citedIn(m).length}>
                    <ol class="nb-cites" aria-label={t("notebooks.citations")}>
                      <For each={citedIn(m)}>
                        {(c, i) => (
                          <li>
                            <button type="button" class="nb-cite-row" onClick={() => openCitation(m, c.ref)}>
                              <span class="md-cite" aria-hidden="true">{i() + 1}</span>
                              <span class="nb-cite-title">{c.title}</span>
                              <Show when={c.page}>
                                <span class="nb-cite-page">{t("notebooks.pageN", { n: c.page! })}</span>
                              </Show>
                            </button>
                          </li>
                        )}
                      </For>
                    </ol>
                  </Show>
                  <div class="nb-msg-foot">
                    <Show when={m.model}>
                      <span class="pg-muted">{modelLabel(m.model)}</span>
                    </Show>
                    <Show
                      when={saved()[m.id]}
                      fallback={
                        <button type="button" class="pg-link" onClick={() => void saveToNote(m)}>
                          {t("notebooks.saveToNote")}
                        </button>
                      }
                    >
                      {(slug) => <a class="pg-link" href={`/keep/note/${slug()}`}>{t("notebooks.savedOpen")}</a>}
                    </Show>
                  </div>
                </>
              }>
                <p class="nb-msg-text">{m.text}</p>
              </Show>
            </li>
          )}
        </For>
        <Show when={pending()}>
          {(p) => (
            <>
              <li class="nb-msg" data-role="user">
                <p class="nb-msg-text">{p().user}</p>
              </li>
              <li class="nb-msg" data-role="assistant" aria-busy="true">
                <Show when={p().reply} fallback={<p class="pg-muted">{t("notebooks.thinking")}</p>}>
                  <Markdown text={p().reply} class="nb-msg-text" />
                </Show>
              </li>
            </>
          )}
        </Show>
      </ol>

      <Show when={error()}>
        <p class="pg-notice nb-chat-error" role="alert">{error()}</p>
      </Show>
      <form
        class="nb-composer"
        onSubmit={(e) => {
          e.preventDefault();
          void send();
        }}
      >
        <textarea
          class="pg-input nb-compose-text"
          rows={2}
          placeholder={t("notebooks.askPlaceholder")}
          aria-label={t("notebooks.askPlaceholder")}
          value={draft()}
          onInput={(e) => setDraft(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
              e.preventDefault();
              void send();
            }
          }}
        />
        <div class="nb-compose-row">
          <select class="pg-input nb-model" aria-label={t("notebooks.model")} onChange={(e) => setModel(e.currentTarget.value as ChatModel)}>
            <option value="deepseek" selected={model() === "deepseek"}>{t("notebooks.modelDeepseek")}</option>
            <option value="qwen" selected={model() === "qwen"}>{t("notebooks.modelQwen")}</option>
          </select>
          <Button type="submit" disabled={!draft().trim() || sending()}>
            <SendIcon /> {t("notebooks.send")}
          </Button>
        </div>
      </form>
    </Card>
  );
}
