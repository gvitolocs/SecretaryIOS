import { createEffect, createMemo, createSignal, For, Match, on, onCleanup, onMount, Show, Switch } from "solid-js";
import { CHAT_MAX_CHARS } from "../api/secretary";
import type { Attachment, Channel } from "../api/timeline";
import {
  AvatarIcon,
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
import { LucinaPanel } from "../components/LucinaPanel";
import { safeHref } from "../lib/linkify";
import { readString, writeString } from "../lib/storage";
import {
  CHANNEL_LABELS,
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
  retryMessage,
  sending,
  sendMessage,
  streamStatus,
  type ChatMessage,
  type JobState,
} from "../state/chat";

const JOB_LABELS: Record<string, string> = {
  queued: "Queued",
  running: "Working on it",
  done: "Done",
  error: "Failed",
};

const JOB_ERRORS: Record<string, string> = {
  interrupted: "Lucina restarted while working on this.",
};

const STREAM_LABELS = {
  connecting: "Connecting…",
  live: "Live",
  reconnecting: "Reconnecting…",
  offline: "Not live",
} as const;

const ATTACHMENT_LABELS: Record<Attachment["kind"], string> = {
  image: "Photo",
  voice: "Voice message",
  file: "File",
};

const STAGE_HIDDEN_KEY = "secretary.stage.hidden";

/** Start loading older items when the list is scrolled this close to the top. */
const LOAD_OLDER_PX = 200;
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
      {(t) => <LinkifiedText class="job-result" text={t()} />}
    </Show>
  );
}

function JobCard(props: { job: JobState }) {
  const label = () => JOB_LABELS[props.job.status] ?? props.job.status;
  const done = () => isTerminalJob(props.job.status);
  return (
    <section
      class="job"
      classList={{
        "job--done": props.job.status === "done",
        "job--error": props.job.status === "error",
        "job--active": !done(),
      }}
      aria-label={`Job: ${label()}`}
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
        {(e) => <p class="job-note">{JOB_ERRORS[e()] ?? e()}</p>}
      </Show>
      <Show when={props.job.imageUrl ? safeHref(props.job.imageUrl) : null}>
        {(href) => (
          <a class="job-link" href={href()} target="_blank" rel="noopener noreferrer nofollow">
            Open image
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
  const label = () => CHANNEL_LABELS[props.message.channel];
  const context = () => contextCaption(props.message.channel, props.message.meta);
  return (
    <span class="src-badge" data-channel={props.message.channel}>
      <ChannelGlyph channel={props.message.channel} />
      <span class="sr-only">via </span>
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
    <ul class="attachments" aria-label="Attachments">
      <For each={props.items}>
        {(a) => (
          <li class="attachment">
            {ATTACHMENT_LABELS[a.kind]}
            <Show when={a.name}>{(n) => <span class="attachment-name">: {n()}</span>}</Show>
          </li>
        )}
      </For>
    </ul>
  );
}

function MessageItem(props: { message: ChatMessage }) {
  const [showTime, setShowTime] = createSignal(false);
  const isUser = () => props.message.role === "user";
  const isReminder = () => props.message.meta?.kind === "reminder";
  const hasBody = () => !!props.message.text || !!props.message.attachments?.length;

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
      <span class="sr-only">{isReminder() ? "Reminder:" : isUser() ? "You:" : "Lucina:"}</span>
      <Show when={hasBody() || isReminder()}>
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
              Reminder
            </span>
          </Show>
          <Show when={props.message.text}>
            <LinkifiedText text={props.message.text} />
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
          <span class="msg-imported">Imported from history</span>
        </Show>
        <time
          class="msg-time"
          dateTime={new Date(props.message.createdAt).toISOString()}
          title={fullTimeLabel(props.message.createdAt)}
        >
          {timeLabel(props.message.createdAt)}
        </time>
        <Show when={props.message.status === "sending"}>
          <span class="msg-meta">Sending…</span>
        </Show>
      </div>
      <Show when={props.message.status === "error"}>
        <div class="msg-error">
          <span>{props.message.error ?? "Not delivered."}</span>
          <button
            type="button"
            class="btn btn--small"
            onClick={() => void retryMessage(props.message.key)}
            disabled={sending()}
          >
            Retry
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
      title={streamStatus() === "live" ? "New messages from every channel appear as they arrive." : undefined}
    >
      <span class="live-dot" aria-hidden="true" />
      {STREAM_LABELS[streamStatus()]}
    </span>
  );
}

export default function ChatView() {
  const [draft, setDraft] = createSignal("");
  const [filter, setFilter] = createSignal<SourceFilter>("all");
  const [stageOpen, setStageOpen] = createSignal(readString("local", STAGE_HIDDEN_KEY) !== "1");
  const tooLong = createMemo(() => draft().length > CHAT_MAX_CHARS);
  const canSend = createMemo(() => draft().trim().length > 0 && !tooLong() && !sending());
  const visible = createMemo(() => {
    const f = filter();
    return f === "all" ? chat.messages : chat.messages.filter((m) => matchesFilter(m.channel, f));
  });
  const filterLabel = () => SOURCE_FILTERS.find((f) => f.id === filter())?.label ?? "";
  let list: HTMLDivElement | undefined;
  let input: HTMLTextAreaElement | undefined;
  let atBottom = true;

  function autosize() {
    if (!input) return;
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight, 160)}px`;
  }

  function scrollToEnd(smooth: boolean) {
    if (!list) return;
    const reduce = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
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
    if (!list) return;
    atBottom = list.scrollHeight - list.scrollTop - list.clientHeight < STICK_PX;
    if (list.scrollTop < LOAD_OLDER_PX && hasMore()) void older();
  }

  onMount(() => {
    scrollToEnd(false);
    onCleanup(connectTimeline());
  });

  // New items at the end (or the thinking bubble) keep the view pinned to the
  // bottom unless the reader has scrolled up. Prepending never moves it.
  createEffect(
    on(
      () => [visible()[visible().length - 1]?.key, sending()] as const,
      () => {
        if (atBottom) requestAnimationFrame(() => scrollToEnd(true));
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

  function showStage(open: boolean) {
    setStageOpen(open);
    writeString("local", STAGE_HIDDEN_KEY, open ? "0" : "1");
  }

  function sendTranscript(text: string) {
    if (filter() !== "all" && filter() !== "app") setFilter("all");
    atBottom = true;
    void sendMessage(text);
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
    <div class="chat" classList={{ "chat--stage": stageOpen() }}>
      <Show when={stageOpen()}>
        <LucinaPanel
          thinking={sending()}
          onTranscript={sendTranscript}
          onTypeInstead={() => input?.focus()}
          onCollapse={() => showStage(false)}
        />
      </Show>
      <div class="chat-main">
        <div class="chat-bar">
          <div class="chip-group" role="group" aria-label="Show messages from">
            <For each={SOURCE_FILTERS}>
              {(f) => (
                <button
                  type="button"
                  class="chip"
                  classList={{ "chip--active": filter() === f.id }}
                  aria-pressed={filter() === f.id ? "true" : "false"}
                  onClick={() => setFilter(f.id)}
                >
                  {f.label}
                </button>
              )}
            </For>
          </div>
          <Show when={!stageOpen()}>
            <button
              type="button"
              class="icon-btn stage-show"
              onClick={() => showStage(true)}
              aria-expanded="false"
              aria-label="Show Lucina"
              title="Show Lucina"
            >
              <AvatarIcon />
            </button>
          </Show>
          <LiveIndicator />
        </div>
        <div
          class="messages"
          ref={list}
          onScroll={onScroll}
          role="log"
          aria-live="off"
          aria-label="Conversation with Lucina"
        >
          <div class="timeline-top">
            <Switch>
              <Match when={olderState() === "loading"}>
                <span class="timeline-hint" role="status">
                  Loading earlier messages…
                </span>
              </Match>
              <Match when={olderState() === "error"}>
                <button type="button" class="btn btn--small" onClick={() => void older()}>
                  Couldn't load earlier messages. Try again
                </button>
              </Match>
              <Match when={hasMore()}>
                <button type="button" class="btn btn--small" onClick={() => void older()}>
                  Load earlier messages
                </button>
              </Match>
              <Match when={chat.loaded && chat.messages.length > 0}>
                <span class="timeline-hint">Start of the conversation</span>
              </Match>
            </Switch>
          </div>
          <Show when={loadState() === "error"}>
            <div class="timeline-error" role="alert">
              <span>Couldn't load the conversation.</span>
              <button type="button" class="btn btn--small" onClick={() => void loadLatest()}>
                Try again
              </button>
            </div>
          </Show>
          <Show when={!chat.loaded && chat.messages.length === 0 && loadState() !== "error"}>
            <p class="timeline-hint" role="status">
              Loading the conversation…
            </p>
          </Show>
          <Show when={chat.loaded && chat.messages.length === 0 && !sending()}>
            <div class="empty">
              <p class="empty-title">Hello. Lucina is listening.</p>
              <p class="empty-sub">Ask her anything, or have her add to your shopping list.</p>
            </div>
          </Show>
          <Show when={chat.messages.length > 0 && visible().length === 0}>
            <p class="timeline-hint">No {filterLabel()} messages loaded yet.</p>
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
                <MessageItem message={m} />
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
                Lucina is thinking…
              </div>
            </div>
          </Show>
        </div>
        <div class="sr-only" aria-live="polite" aria-atomic="true">
          {sending() ? "Lucina is thinking." : lastReply() ? `Lucina: ${lastReply()}` : ""}
        </div>
        <form
          class="composer"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <label class="sr-only" for="composer-input">
            Message Lucina
          </label>
          <textarea
            ref={input}
            id="composer-input"
            class="composer-input"
            rows="1"
            placeholder={sending() ? "Waiting for Lucina…" : "Message Lucina"}
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
          <button class="send-btn" type="submit" disabled={!canSend()} aria-label="Send">
            <SendIcon />
          </button>
        </form>
      </div>
    </div>
  );
}
