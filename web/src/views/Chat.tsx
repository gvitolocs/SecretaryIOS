import { createEffect, createMemo, createSignal, For, on, onMount, Show } from "solid-js";
import { CHAT_MAX_CHARS } from "../api/secretary";
import { SendIcon } from "../components/Icons";
import { LinkifiedText } from "../components/LinkifiedText";
import { safeHref } from "../lib/linkify";
import {
  chat,
  isTerminalJob,
  lastReply,
  retryMessage,
  sending,
  sendMessage,
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

function MessageItem(props: { message: ChatMessage }) {
  const isUser = () => props.message.role === "user";
  return (
    <div class="msg" classList={{ "msg--user": isUser(), "msg--lucina": !isUser() }}>
      <span class="sr-only">{isUser() ? "You:" : "Lucina:"}</span>
      <Show when={props.message.text}>
        <div class="bubble" classList={{ "bubble--failed": props.message.status === "error" }}>
          <LinkifiedText text={props.message.text} />
        </div>
      </Show>
      <Show when={props.message.job}>{(job) => <JobCard job={job()} />}</Show>
      <Show when={props.message.status === "sending"}>
        <span class="msg-meta">Sending…</span>
      </Show>
      <Show when={props.message.status === "error"}>
        <div class="msg-error">
          <span>{props.message.error ?? "Not delivered."}</span>
          <button
            type="button"
            class="btn btn--small"
            onClick={() => void retryMessage(props.message.id)}
            disabled={sending()}
          >
            Retry
          </button>
        </div>
      </Show>
    </div>
  );
}

export default function ChatView() {
  const [draft, setDraft] = createSignal("");
  const tooLong = createMemo(() => draft().length > CHAT_MAX_CHARS);
  const canSend = createMemo(() => draft().trim().length > 0 && !tooLong() && !sending());
  let list: HTMLDivElement | undefined;
  let input: HTMLTextAreaElement | undefined;

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

  onMount(() => scrollToEnd(false));
  createEffect(
    on(
      () => [chat.messages.length, sending()] as const,
      () => requestAnimationFrame(() => scrollToEnd(true)),
      { defer: true },
    ),
  );

  async function submit() {
    if (!canSend()) return;
    const text = draft();
    setDraft("");
    queueMicrotask(autosize);
    await sendMessage(text);
    input?.focus();
  }

  function onKeyDown(e: KeyboardEvent) {
    if (e.key !== "Enter" || e.shiftKey || e.isComposing || e.keyCode === 229) return;
    e.preventDefault();
    void submit();
  }

  return (
    <div class="chat">
      <div class="messages" ref={list} role="log" aria-live="off" aria-label="Conversation with Lucina">
        <Show when={chat.messages.length === 0 && !sending()}>
          <div class="empty">
            <p class="empty-title">Hello. Lucina is listening.</p>
            <p class="empty-sub">Ask her anything, or have her add to your shopping list.</p>
          </div>
        </Show>
        <For each={chat.messages}>{(m) => <MessageItem message={m} />}</For>
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
  );
}
