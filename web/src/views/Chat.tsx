import { createEffect, createMemo, createSignal, For, Match, on, onCleanup, onMount, Show, Switch } from "solid-js";
import { CHAT_MAX_CHARS } from "../api/secretary";
import type { Attachment, Channel } from "../api/timeline";
import {
  BellIcon,
  BubbleIcon,
  DotsIcon,
  GamepadIcon,
  GlobeIcon,
  PhoneIcon,
  SendIcon,
  SpeakerIcon,
} from "../components/Icons";
import { LinkifiedText } from "../components/LinkifiedText";
import { Markdown } from "../components/Markdown";
import { RichReply } from "../components/RichReply";
import { t, type MessageKey } from "../i18n";
import { safeHref } from "../lib/linkify";
import { actionText } from "../lib/rich";
import {
  channelLabel,
  contextCaption,
  dayKey,
  dayLabel,
  fullTimeLabel,
  matchesFilter,
  SOURCE_FILTERS,
  timeLabel,
  type SourceFilter,
} from "../lib/timeline";
import {
  chat,
  connectTimeline,
  hasMore,
  isTerminalJob,
  lastReply,
  loadLatest,
  loadOlder,
  loadState,
  olderState,
  prefill,
  retryMessage,
  revealTick,
  richOf,
  sending,
  sendMessage,
  streamStatus,
  type ChatMessage,
  type JobState,
} from "../state/chat";

const JOB_LABELS: Record<string, MessageKey> = {
  queued: "chat.jobQueued",
  running: "chat.jobRunning",
  done: "chat.jobDone",
  error: "chat.jobFailed",
};

const JOB_ERRORS: Record<string, MessageKey> = {
  interrupted: "chat.jobInterrupted",
};

const STREAM_LABELS = {
  connecting: "chat.streamConnecting",
  live: "chat.streamLive",
  reconnecting: "chat.streamReconnecting",
  offline: "chat.streamOffline",
} as const satisfies Record<string, MessageKey>;

const ATTACHMENT_LABELS: Record<Attachment["kind"], MessageKey> = {
  image: "chat.photo",
  voice: "chat.voice",
  file: "chat.file",
};

/** Start loading older items when the list is scrolled this close to the top. */
const LOAD_OLDER_PX = 200;
/** Long enough for a smooth scroll to the end to finish. */
const AUTO_SCROLL_MS = 900;
/** Within this distance of the bottom, new items keep the list pinned to the end. */
const STICK_PX = 80;

function resultText(value: unknown): string | null {
  if (typeof value === "string") return value;
  if (value && typeof value === "object") {
    for (const key of ["message", "summary", "text", "reply"]) {
      const v = (value as Record<string, unknown>)[key];
      if (typeof v === "string" && v.trim()) return v;
    }
  }
  return null;
}

function JobResult(props: { value: unknown }) {
  const text = () => resultText(props.value);
  return (
    <Show
      when={text()}
      fallback={
        <Show when={props.value !== null && props.value !== undefined}>
          <pre class="job-json">{JSON.stringify(props.value, null, 2)}</pre>
        </Show>
      }
    >
      {(text) => <LinkifiedText class="job-result" text={text()} />}
    </Show>
  );
}

function JobCard(props: { job: JobState }) {
  const label = () => {
    const key = JOB_LABELS[props.job.status];
    return key ? t(key) : props.job.status;
  };
  const done = () => isTerminalJob(props.job.status);
  return (
    <section
      class="job"
      classList={{
        "job--done": props.job.status === "done",
        "job--error": props.job.status === "error",
        "job--active": !done(),
      }}
      aria-label={t("chat.jobLabel", { status: label() })}
    >
      <div class="job-head">
        <Show when={!done()}>
          <span class="spinner" aria-hidden="true" />
        </Show>
        <span class="job-status">{label()}</span>
        <Show when={props.job.action}>{(a) => <span class="job-action">{a()}</span>}</Show>
      </div>
      <Show when={done() && props.job.result !== undefined && props.job.result !== null}>
        <JobResult value={props.job.result} />
      </Show>
      <Show when={props.job.status === "error" && props.job.error}>
        {(e) => <p class="job-note">{JOB_ERRORS[e()] ? t(JOB_ERRORS[e()]!) : e()}</p>}
      </Show>
      <Show when={props.job.imageUrl ? safeHref(props.job.imageUrl) : null}>
        {(href) => (
          <a class="job-link" href={href()} target="_blank" rel="noopener noreferrer nofollow">
            {t("chat.openImage")}
          </a>
        )}
      </Show>
      <Show when={props.job.pollError}>{(e) => <p class="job-note">{e()}</p>}</Show>
    </section>
  );
}

function ChannelGlyph(props: { channel: Channel }) {
  return (
    <Switch fallback={<DotsIcon />}>
      <Match when={props.channel === "web"}>
        <GlobeIcon />
      </Match>
      <Match when={props.channel === "ios"}>
        <PhoneIcon />
      </Match>
      <Match when={props.channel === "telegram"}>
        <BubbleIcon />
      </Match>
      <Match when={props.channel === "discord"}>
        <GamepadIcon />
      </Match>
      <Match when={props.channel === "alexa"}>
        <SpeakerIcon />
      </Match>
    </Switch>
  );
}

/** "via Telegram · Family" badge: glyph plus a text label, read as one phrase. */
export function SourceBadge(props: { message: ChatMessage }) {
  const label = () => channelLabel(props.message.channel);
  const context = () => contextCaption(props.message.channel, props.message.meta);
  return (
    <span class="src-badge" data-channel={props.message.channel}>
      <ChannelGlyph channel={props.message.channel} />
      <span class="sr-only">{t("chat.via")} </span>
      <span class="src-label">{label()}</span>
      <Show when={context()}>
        {(c) => (
          <span class="src-context">
            <span aria-hidden="true"> · </span>
            {c()}
          </span>
        )}
      </Show>
    </span>
  );
}

function Attachments(props: { items: Attachment[] }) {
  return (
    <ul class="attachments" aria-label={t("chat.attachments")}>
      <For each={props.items}>
        {(a) => (
          <li class="attachment">
            {t(ATTACHMENT_LABELS[a.kind])}
            <Show when={a.name}>{(n) => <span class="attachment-name">: {n()}</span>}</Show>
          </li>
        )}
      </For>
    </ul>
  );
}

function MessageItem(props: { message: ChatMessage; inferredChoice?: string }) {
  const [showTime, setShowTime] = createSignal(false);
  const isUser = () => props.message.role === "user";
  const isReminder = () => props.message.meta?.kind === "reminder";
  // Lucina message format v1: bubbles, widgets and buttons (not for reminders).
  const rich = () => (!isUser() && !isReminder() ? richOf(props.message) : undefined);
  const hasBody = () => !rich() && (!!props.message.text || !!props.message.attachments?.length);

  function toggleTime(e: MouseEvent) {
    // Taps on links open the link; everything else reveals the time.
    if ((e.target as Element | null)?.closest?.("a")) return;
    setShowTime((v) => !v);
  }

  return (
    <div
      class="msg"
      classList={{
        "msg--user": isUser() && !isReminder(),
        "msg--lucina": !isUser() && !isReminder(),
        "msg--reminder": isReminder(),
        "msg--backfilled": !!props.message.backfilled,
        "msg--show-time": showTime(),
      }}
      data-key={props.message.key}
    >
      <span class="sr-only">{isReminder() ? t("chat.reminderSr") : isUser() ? t("chat.you") : t("chat.lucina")}</span>
      <Show when={rich()}>
        {(r) => <RichReply message={props.message} rich={r()} onBubbleClick={toggleTime} inferredChoice={props.inferredChoice} />}
      </Show>
      <Show when={rich() && props.message.attachments?.length ? props.message.attachments : null}>
        {(a) => (
          <div class="bubble">
            <Attachments items={a()} />
          </div>
        )}
      </Show>
      <Show when={hasBody() || (isReminder() && !rich())}>
        <div
          class="bubble"
          classList={{
            "bubble--failed": props.message.status === "error",
            "bubble--reminder": isReminder(),
          }}
          onClick={toggleTime}
        >
          <Show when={isReminder()}>
            <span class="reminder-head" aria-hidden="true">
              <BellIcon />
              {t("chat.reminder")}
            </span>
          </Show>
          <Show when={props.message.text}>
            {/* Her plain replies are markdown too (safe renderer from Notes): no more literal "**". */}
            <Show when={isUser()} fallback={<Markdown text={props.message.text} class="md--chat" />}>
              <LinkifiedText text={props.message.text} />
            </Show>
          </Show>
          <Show when={props.message.attachments?.length ? props.message.attachments : null}>
            {(a) => <Attachments items={a()} />}
          </Show>
        </div>
      </Show>
      <Show when={props.message.job}>{(job) => <JobCard job={job()} />}</Show>
      <div class="msg-info">
        <SourceBadge message={props.message} />
        <Show when={props.message.backfilled}>
          <span class="msg-imported">{t("chat.imported")}</span>
        </Show>
        <time
          class="msg-time"
          dateTime={new Date(props.message.createdAt).toISOString()}
          title={fullTimeLabel(props.message.createdAt)}
        >
          {timeLabel(props.message.createdAt)}
        </time>
        <Show when={props.message.status === "sending"}>
          <span class="msg-meta">{t("chat.sending")}</span>
        </Show>
      </div>
      <Show when={props.message.status === "error"}>
        <div class="msg-error">
          <span>{props.message.error ?? t("chat.notDelivered")}</span>
          <button
            type="button"
            class="btn btn--small"
            onClick={() => void retryMessage(props.message.key)}
            disabled={sending()}
          >
            {t("chat.retry")}
          </button>
        </div>
      </Show>
    </div>
  );
}

function LiveIndicator() {
  return (
    <span
      class="live"
      classList={{ "live--on": streamStatus() === "live" }}
      role="status"
      title={streamStatus() === "live" ? t("chat.liveTitle") : undefined}
    >
      <span class="live-dot" aria-hidden="true" />
      {t(STREAM_LABELS[streamStatus()])}
    </span>
  );
}

export default function ChatView() {
  const [draft, setDraft] = createSignal("");
  const [filter, setFilter] = createSignal<SourceFilter>("all");
  const tooLong = createMemo(() => draft().length > CHAT_MAX_CHARS);
  const canSend = createMemo(() => draft().trim().length > 0 && !tooLong() && !sending());
  const visible = createMemo(() => {
    const f = filter();
    return f === "all" ? chat.messages : chat.messages.filter((m) => matchesFilter(m.channel, f));
  });
  const filterLabel = () => {
    const key = SOURCE_FILTERS.find((f) => f.id === filter())?.label;
    return key ? t(key) : "";
  };
  /**
   * Buttons the owner answered by typing: a Lucina message whose next owner
   * message equals one of its buttons' text counts as that choice (survives
   * reloads and other devices).
   */
  const inferredChoices = createMemo(() => {
    const out = new Map<string, string>();
    const items = chat.messages;
    for (let i = 0; i < items.length; i++) {
      const m = items[i]!;
      const rich = m.role === "lucina" ? richOf(m) : undefined;
      if (!rich?.actions.length) continue;
      const next = items.slice(i + 1).find((x) => x.role === "user");
      const said = next?.text.trim().toLowerCase();
      if (!said) continue;
      const hit = rich.actions.find((a) => actionText(a)?.trim().toLowerCase() === said);
      if (hit) out.set(m.key, hit.id);
    }
    return out;
  });
  let list: HTMLDivElement | undefined;
  let input: HTMLTextAreaElement | undefined;
  let atBottom = true;
  /** While our own scroll-to-end runs, scroll events are not the reader's. */
  let autoScrollUntil = 0;

  function autosize() {
    if (!input) return;
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight, 160)}px`;
  }

  function scrollToEnd(smooth: boolean) {
    if (!list) return;
    const reduce = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
    autoScrollUntil = Date.now() + (smooth && !reduce ? AUTO_SCROLL_MS : 100);
    if (typeof list.scrollTo === "function") {
      list.scrollTo({ top: list.scrollHeight, behavior: smooth && !reduce ? "smooth" : "auto" });
    } else {
      list.scrollTop = list.scrollHeight;
    }
  }

  /** Prepend the previous page while keeping the visible messages where they are. */
  async function older() {
    if (!list || !hasMore() || olderState() === "loading") return;
    const height = list.scrollHeight;
    const top = list.scrollTop;
    const added = await loadOlder();
    if (added > 0 && list) list.scrollTop = top + (list.scrollHeight - height);
  }

  function onScroll() {
    if (!list || Date.now() < autoScrollUntil) return;
    atBottom = list.scrollHeight - list.scrollTop - list.clientHeight < STICK_PX;
    if (list.scrollTop < LOAD_OLDER_PX && hasMore()) void older();
  }

  onMount(() => {
    scrollToEnd(false);
    onCleanup(connectTimeline());
  });

  // New items at the end (or the thinking bubble) keep the view pinned to the
  // bottom unless the reader has scrolled up. Prepending never moves it. The
  // first fill jumps straight to the end: a smooth scroll from the top would
  // pass the load-older zone on its way down.
  createEffect(
    on(
      () => [visible()[visible().length - 1]?.key, sending()] as const,
      (_now, prev) => {
        const firstFill = !prev?.[0];
        if (atBottom) requestAnimationFrame(() => scrollToEnd(!firstFill));
      },
      { defer: true },
    ),
  );

  // A revealing reply grows bubble by bubble: stay pinned to the end.
  createEffect(
    on(
      revealTick,
      () => {
        if (atBottom) requestAnimationFrame(() => scrollToEnd(true));
      },
      { defer: true },
    ),
  );

  // A button asked for text in the composer ("Ricordami alle 21:30 di …", "Modifica").
  createEffect(
    on(
      prefill,
      (p) => {
        if (!p) return;
        const text = p.text && !/\s$/.test(p.text) ? `${p.text} ` : p.text;
        setDraft(text);
        queueMicrotask(() => {
          autosize();
          input?.focus();
          input?.setSelectionRange(text.length, text.length);
        });
      },
      { defer: true },
    ),
  );

  createEffect(
    on(
      filter,
      () => {
        atBottom = true;
        requestAnimationFrame(() => scrollToEnd(false));
      },
      { defer: true },
    ),
  );

  async function submit() {
    if (!canSend()) return;
    const text = draft();
    setDraft("");
    queueMicrotask(autosize);
    // Your own message must be visible: leave a filter that would hide it.
    if (filter() !== "all" && filter() !== "app") setFilter("all");
    atBottom = true;
    await sendMessage(text);
    input?.focus();
  }

  function onKeyDown(e: KeyboardEvent) {
    if (e.key !== "Enter" || e.shiftKey || e.isComposing || e.keyCode === 229) return;
    e.preventDefault();
    void submit();
  }

  /** Day separator before message `i` of the visible list. */
  function dayBreak(i: number): string | null {
    const items = visible();
    const m = items[i];
    if (!m) return null;
    const prev = items[i - 1];
    return !prev || dayKey(prev.createdAt) !== dayKey(m.createdAt) ? dayLabel(m.createdAt) : null;
  }

  return (
    <div class="chat">
      <div class="chat-bar">
        <div class="chip-group" role="group" aria-label={t("chat.filterGroup")}>
          <For each={SOURCE_FILTERS}>
            {(f) => (
              <button
                type="button"
                class="chip"
                classList={{ "chip--active": filter() === f.id }}
                aria-pressed={filter() === f.id ? "true" : "false"}
                onClick={() => setFilter(f.id)}
              >
                {t(f.label)}
              </button>
            )}
          </For>
        </div>
        <LiveIndicator />
      </div>
      <div
        class="messages"
        ref={list}
        onScroll={onScroll}
        role="log"
        aria-live="off"
        aria-label={t("chat.conversation")}
      >
        <div class="timeline-top">
          <Switch>
            <Match when={olderState() === "loading"}>
              <span class="timeline-hint" role="status">
                {t("chat.loadingEarlier")}
              </span>
            </Match>
            <Match when={olderState() === "error"}>
              <button type="button" class="btn btn--small" onClick={() => void older()}>
                {t("chat.earlierFailed")}
              </button>
            </Match>
            <Match when={hasMore()}>
              <button type="button" class="btn btn--small" onClick={() => void older()}>
                {t("chat.loadEarlier")}
              </button>
            </Match>
            <Match when={chat.loaded && chat.messages.length > 0}>
              <span class="timeline-hint">{t("chat.start")}</span>
            </Match>
          </Switch>
        </div>
        <Show when={loadState() === "error"}>
          <div class="timeline-error" role="alert">
            <span>{t("chat.loadFailed")}</span>
            <button type="button" class="btn btn--small" onClick={() => void loadLatest()}>
              {t("common.tryAgain")}
            </button>
          </div>
        </Show>
        <Show when={!chat.loaded && chat.messages.length === 0 && loadState() !== "error"}>
          <p class="timeline-hint" role="status">
            {t("chat.loading")}
          </p>
        </Show>
        <Show when={chat.loaded && chat.messages.length === 0 && !sending()}>
          <div class="empty">
            <p class="empty-title">{t("chat.emptyTitle")}</p>
            <p class="empty-sub">{t("chat.emptySub")}</p>
          </div>
        </Show>
        <Show when={chat.messages.length > 0 && visible().length === 0}>
          <p class="timeline-hint">{t("chat.noFiltered", { source: filterLabel() })}</p>
        </Show>
        <For each={visible()}>
          {(m, i) => (
            <>
              <Show when={dayBreak(i())}>
                {(label) => (
                  <div class="day-sep" role="separator" aria-label={label()}>
                    <span>{label()}</span>
                  </div>
                )}
              </Show>
              <MessageItem message={m} inferredChoice={inferredChoices().get(m.key)} />
            </>
          )}
        </For>
        <Show when={sending()}>
          <div class="msg msg--lucina">
            <div class="bubble bubble--thinking">
              <span class="typing" aria-hidden="true">
                <span />
                <span />
                <span />
              </span>
              {t("chat.thinking")}
            </div>
          </div>
        </Show>
      </div>
      <div class="sr-only" aria-live="polite" aria-atomic="true">
        {sending() ? t("chat.thinkingSr") : lastReply() ? t("chat.replySr", { text: lastReply() }) : ""}
      </div>
      <form
        class="composer"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <label class="sr-only" for="composer-input">
          {t("chat.composer")}
        </label>
        <textarea
          ref={input}
          id="composer-input"
          class="composer-input"
          rows="1"
          placeholder={sending() ? t("chat.waiting") : t("chat.composer")}
          value={draft()}
          onInput={(e) => {
            setDraft(e.currentTarget.value);
            autosize();
          }}
          onKeyDown={onKeyDown}
          readOnly={sending()}
          aria-disabled={sending() ? "true" : "false"}
          enterkeyhint="send"
          autocomplete="off"
        />
        <Show when={tooLong()}>
          <span class="composer-count" role="status">
            {draft().length}/{CHAT_MAX_CHARS}
          </span>
        </Show>
        <button class="send-btn" type="submit" disabled={!canSend()} aria-label={t("chat.send")}>
          <SendIcon />
        </button>
      </form>
    </div>
  );
}
