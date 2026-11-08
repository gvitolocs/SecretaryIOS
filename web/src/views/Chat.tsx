import { InAppLink } from "../components/InAppLink";
import { useLocation, useNavigate } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, Match, on, onCleanup, onMount, Show, Switch } from "solid-js";
import { outreachFromSearch, sendOutreachFeedback } from "../api/proactive";
import { CHAT_MAX_CHARS } from "../api/secretary";
import {
  FEEDBACK_ACTIONS,
  type Channel,
  type FeedbackAction,
  type ProactiveAction,
  type ProactiveMeta,
  type ProactivePriority,
} from "../api/timeline";
import {
  BellIcon,
  BubbleIcon,
  DotsIcon,
  GamepadIcon,
  GlobeIcon,
  MicIcon,
  PhoneIcon,
  SendIcon,
  SparkleIcon,
  SpeakerIcon,
} from "../components/Icons";
import { ShareComposer } from "../components/ShareComposer";
import { VoiceNoteButton } from "../components/VoiceNoteButton";
import { pendingNotes, stillPending } from "../state/voiceNotes";
import { secretsApi, type SecretProject } from "../api/secrets";
import {
  commandArgIndex,
  detectSecrets,
  isSecretCommand,
  maskSecrets,
  parseSecretCommand,
  SECRET_COMMAND,
  SECRET_USAGE,
  suggestName,
  targetCompletions,
  valueHint,
  type Detected,
} from "../lib/secrets";
import { openSecretSheet } from "../state/secretSheet";
import { AttachmentTray } from "../components/AttachmentTray";
import { ToolSteps } from "../components/ToolSteps";
import { chatRows, isToolRun, type ToolRun } from "../lib/toolSteps";
import "../styles/tool-steps.css";
import { Attachments } from "../components/ChatAttachments";
import {
  filesFromClipboard,
  filesFromDataTransfer,
  formatCoordinate,
  hasFileTransfer,
  prepareDraftFiles,
  releaseDraft,
  type DraftAttachment,
  type ShareLocation,
} from "../lib/attachments";
import { takeHandOff } from "../lib/chatHandoff";
import { LinkifiedText } from "../components/LinkifiedText";
import { tidyDecisionText } from "../lib/decisionText";
import { Markdown } from "../components/Markdown";
import { PlaceCard } from "../components/PlaceCard";
import { RichReply } from "../components/RichReply";
import Model3dViewer from "../components/Model3dViewer";
import { markFirstMessage } from "../lib/chatPerf";
import { firstMessageSource, replyTarget, setReplyTarget } from "../state/chat";
import { t, type MessageKey } from "../i18n";
import { safeHref } from "../lib/linkify";
import type { PlaceInfo } from "../lib/mapKit";
import { actionText } from "../lib/rich";
import {
  channelLabel,
  contextCaption,
  dayKey,
  dayLabel,
  fullTimeLabel,
  matchesFilter,
  mcpSource,
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
  sendAttachmentMessage,
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

const PRIORITY_LABELS = {
  high: "chat.priorityHigh",
  normal: "chat.priorityNormal",
  low: "chat.priorityLow",
} as const satisfies Record<ProactivePriority, MessageKey>;

const FEEDBACK_LABELS = {
  useful: "chat.fbUseful",
  not_now: "chat.fbNotNow",
  stop: "chat.fbStop",
} as const satisfies Record<FeedbackAction, MessageKey>;

const FEEDBACK_DONE = {
  useful: "chat.fbThanks",
  not_now: "chat.fbNotNowDone",
  stop: "chat.fbStopDone",
} as const satisfies Record<FeedbackAction, MessageKey>;

/** In-app navigation when a router is around (tests render the view bare). */
function useOptionalNavigate(): ReturnType<typeof useNavigate> | null {
  try {
    return useNavigate();
  } catch {
    return null;
  }
}

function useOptionalLocation(): ReturnType<typeof useLocation> | null {
  try {
    return useLocation();
  } catch {
    return null;
  }
}

/** "Utile / Non ora / Basta così": labels from the message (her language), else ours. */
export function FeedbackButtons(props: { outreachId: string; actions?: ProactiveAction[] }) {
  const [chosen, setChosen] = createSignal<FeedbackAction | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  const buttons = () =>
    FEEDBACK_ACTIONS.map((id) => ({
      id,
      label: props.actions?.find((a) => a.feedback && a.id === id)?.label ?? t(FEEDBACK_LABELS[id]),
    }));

  async function tap(id: FeedbackAction) {
    if (busy() || chosen()) return;
    setBusy(true);
    setFailed(false);
    try {
      await sendOutreachFeedback(props.outreachId, id);
      setChosen(id);
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="proactive-feedback" role="group" aria-label={t("chat.feedbackGroup")}>
      <For each={buttons()}>
        {(b) => (
          <button
            type="button"
            class="chip proactive-fb"
            classList={{ "chip--active": chosen() === b.id }}
            aria-pressed={chosen() === b.id ? "true" : "false"}
            disabled={busy() || chosen() !== null}
            onClick={() => void tap(b.id)}
          >
            {b.label}
          </button>
        )}
      </For>
      <Show when={chosen()}>
        {(c) => (
          <span class="proactive-note" role="status">
            {t(FEEDBACK_DONE[c()])}
          </span>
        )}
      </Show>
      <Show when={failed()}>
        <span class="proactive-note proactive-note--error" role="alert">
          {t("chat.fbFailed")}
        </span>
      </Show>
    </div>
  );
}

/** A message Lucina sent on her own: priority, content buttons, feedback, why. */
function ProactiveCard(props: { text: string; meta: ProactiveMeta; place?: PlaceInfo; readOnly?: boolean }) {
  const navigate = useOptionalNavigate();
  const [showWhy, setShowWhy] = createSignal(false);
  const links = () => props.meta.actions.filter((a) => !a.feedback && a.route).slice(0, 2);

  function open(a: ProactiveAction, e: MouseEvent) {
    if (props.readOnly) return;
    void sendOutreachFeedback(props.meta.outreachId, "opened").catch(() => undefined);
    if (navigate && a.route) {
      e.preventDefault();
      navigate(a.route);
    }
  }

  return (
    <section
      class="proactive"
      classList={{ "proactive--high": props.meta.priority === "high" }}
      data-priority={props.meta.priority}
      aria-label={t("chat.proactiveHead")}
    >
      <header class="proactive-head">
        <span class="proactive-title">
          <SparkleIcon />
          {t("chat.proactiveHead")}
        </span>
        <span class="prio-chip" data-priority={props.meta.priority}>
          {t(PRIORITY_LABELS[props.meta.priority])}
        </span>
      </header>
      <LinkifiedText class="proactive-text" text={props.text} />
      <Show when={props.place}>{(place) => <PlaceCard place={place()} class="proactive-place" />}</Show>
      <Show when={links().length > 0}>
        <div class="proactive-actions">
          <For each={links()}>
            {(a) => (
              <a class="btn btn--small proactive-link" href={a.route} onClick={(e) => open(a, e)}>
                {a.label}
              </a>
            )}
          </For>
        </div>
      </Show>
      <Show when={!props.readOnly}><FeedbackButtons outreachId={props.meta.outreachId} actions={props.meta.actions} /></Show>
      <Show when={props.meta.why}>
        {(why) => (
          <div class="proactive-why">
            <button
              type="button"
              class="proactive-why-btn"
              aria-expanded={showWhy() ? "true" : "false"}
              onClick={() => setShowWhy((v) => !v)}
            >
              {t("chat.proactiveWhy")}
            </button>
            <Show when={showWhy()}>
              <p class="proactive-why-text" title={t("chat.proactiveWhyTitle")}>
                {why()}
              </p>
            </Show>
          </div>
        )}
      </Show>
    </section>
  );
}

/** Start loading older items when the list is scrolled this close to the top. */
const LOAD_OLDER_PX = 200;
/** Long enough for a smooth scroll to the end to finish. */
const AUTO_SCROLL_MS = 900;
/** Within this distance of the bottom, new items keep the list pinned to the end. */
const STICK_PX = 80;
/** A scroll this soon after the reader's own input (wheel, touch, keys, scrollbar drag) is theirs. */
const READER_INPUT_MS = 700;
/** How long reader input waits for its first scroll event (a busy page can paint that late). */
const READER_PENDING_MS = 3_000;

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
      <Match when={props.channel === "voice"}>
        <MicIcon />
      </Match>
    </Switch>
  );
}

/** "via Telegram · Family" badge: glyph plus a text label, read as one phrase. */
export function SourceBadge(props: { message: ChatMessage }) {
  const source = () => mcpSource(props.message.role, props.message.text, props.message.meta);
  const label = () => source()?.label ?? channelLabel(props.message.channel);
  const context = () => (source() ? "" : contextCaption(props.message.channel, props.message.meta));
  // "Other" tells the owner nothing: no badge then (the reminder card already says what it is).
  return (
    <Show when={source() || props.message.channel !== "other"}>
    <span class="src-badge" data-channel={source() ? "mcp" : props.message.channel}>
      <Show when={source()} fallback={<ChannelGlyph channel={props.message.channel} />}>
        <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M9 3h6v4h1a3 3 0 1 1 0 6h-1v8H9v-4H8a3 3 0 1 1 0-6h1V3z" />
        </svg>
      </Show>
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
    </Show>
  );
}

/** "Jev · 93% · 412 ms": a reply Jev decided (meta.jev), with its certainty and latency. */
export function JevBadge(props: { jev: NonNullable<ChatMessage["meta"]>["jev"] }) {
  const parts = () => {
    const j = props.jev;
    if (!j) return [];
    const out = ["Jev"];
    if (typeof j.p === "number" && (j.answer === "yes" || j.answer === "no")) out.push(`${Math.round(j.p * 100)}%`);
    if (typeof j.ms === "number") out.push(`${j.ms} ms`);
    return out;
  };
  return (
    <span class="jev-badge" title={t("chat.jevBadge")} data-answer={props.jev?.answer}>
      <span class="sr-only">{t("chat.jevBadge")}: </span>
      {parts().join(" · ")}
    </span>
  );
}

export { Attachments };

export function MessageItem(props: { message: ChatMessage; focused?: boolean; inferredChoice?: string; readOnly?: boolean }) {
  const [showTime, setShowTime] = createSignal(false);
  const isUser = () => props.message.role === "user";
  const isReminder = () => props.message.meta?.kind === "reminder";
  const proactive = () =>
    props.message.role === "lucina" && props.message.meta?.kind === "proactive" ? props.message.meta.proactive : undefined;
  // Lucina message format v1: bubbles, widgets and buttons (not for reminders or proactive cards).
  const rich = () => (!isUser() && !isReminder() && !proactive() ? richOf(props.message) : undefined);
  const text = () => mcpSource(props.message.role, props.message.text, props.message.meta)?.text ?? props.message.text;
  const hasBody = () => !rich() && (!!text() || !!props.message.attachments?.length);
  // A photo album's caption sits under it, Telegram-style.
  const captionUnder = () => isUser() && !!props.message.attachments?.some((a) => a.kind === "image");
  const shareStatus = () => {
    const share = props.message.share;
    if (!share) return "";
    if (share.phase === "uploading") return t("share.uploadProgress", { percent: Math.round(share.progress * 100) });
    if (share.phase === "error" || share.phase === "done") return "";
    return t("share.reading");
  };
  const uploading = () => props.message.share?.phase === "uploading";

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
        "msg--proactive": !!proactive(),
        "msg--focus": !!props.focused,
        "msg--backfilled": !!props.message.backfilled,
        "msg--show-time": showTime(),
      }}
      data-key={props.message.key}
      data-outreach={proactive()?.outreachId}
    >
      <span class="sr-only">{isReminder() ? t("chat.reminderSr") : isUser() ? t("chat.you") : t("chat.lucina")}</span>
      <Show when={proactive()}>{(p) => <ProactiveCard readOnly={props.readOnly} text={props.message.text} meta={p()} place={props.message.meta?.place} />}</Show>
      <Show when={rich()}>
        {(r) => <RichReply readOnly={props.readOnly} message={props.message} rich={r()} onBubbleClick={toggleTime} inferredChoice={props.inferredChoice} />}
      </Show>
      <Show when={rich() && props.message.attachments?.length ? props.message.attachments : null}>
        {(a) => (
          <div class="bubble">
            <Attachments items={a()} />
          </div>
        )}
      </Show>
      <Show when={!proactive() && (hasBody() || (isReminder() && !rich()))}>
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
          <Show when={captionUnder() && props.message.attachments?.length ? props.message.attachments : null}>
            {(a) => <Attachments items={a()} />}
          </Show>
          <Show when={text()}>
            {/* Her plain replies are markdown too (safe renderer from Notes): no more literal "**". */}
            <Show when={isUser()} fallback={<Markdown text={tidyDecisionText(props.message.text)} class="md--chat" />}>
              <Show when={captionUnder()} fallback={<LinkifiedText text={text()} />}>
                <div class="bubble-caption"><LinkifiedText text={text()} /></div>
              </Show>
            </Show>
          </Show>
          <Show when={!captionUnder() && props.message.attachments?.length ? props.message.attachments : null}>
            {(a) => <Attachments items={a()} />}
          </Show>
          <Show when={shareStatus()}>
            <p class="bubble-status" classList={{ "bubble-status--uploading": uploading() }} role="status">
              <Show when={uploading()}>
                <span
                  class="share-progress-ring"
                  style={{ "--share-progress": `${Math.round((props.message.share?.progress ?? 0) * 100)}%` }}
                  aria-hidden="true"
                />
              </Show>
              {shareStatus()}
            </p>
          </Show>
        </div>
      </Show>
      {/* The note Lucina saved or read (meta.kind "note"): one tap into Keep. */}
      <Show when={props.message.meta?.kind === "note" && props.message.meta.slug}>
        {(slug) => (
          <InAppLink href={`/keep/note/${slug()}`} class="msg-note-link">
            {t("chat.openNote")}
          </InAppLink>
        )}
      </Show>
      <For each={props.message.meta?.blocks ?? []}>{(b) => <Model3dViewer block={b} />}</For>
      <Show when={props.message.job}>{(job) => <JobCard job={job()} />}</Show>
      <div class="msg-info">
        <SourceBadge message={props.message} />
        <Show when={!isUser() && props.message.meta?.jev}>{(j) => <JevBadge jev={j()} />}</Show>
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
        <Show when={props.message.status === "sending" && !props.message.share}>
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

/**
 * The stream state for screen readers only: the header status dot is the one visible connection
 * signal (as on iOS).
 */
function LiveIndicator() {
  return (
    <span class="sr-only" role="status">
      {t(STREAM_LABELS[streamStatus()])}
    </span>
  );
}

export default function ChatView() {
  createEffect(() => {
    if (chat.messages.length) queueMicrotask(() => {
      if (document.querySelector(".msg")) markFirstMessage(firstMessageSource);
    });
  });
  const [draft, setDraft] = createSignal("");
  const [filter, setFilter] = createSignal<SourceFilter>("all");
  const [drafts, setDrafts] = createSignal<DraftAttachment[]>([]);
  const [attachLocation, setAttachLocation] = createSignal<ShareLocation | null>(null);
  const [attachOpen, setAttachOpen] = createSignal(false);
  const [attachError, setAttachError] = createSignal("");
  const [dragging, setDragging] = createSignal(false);
  let dragDepth = 0;
  let photosInput: HTMLInputElement | undefined;
  let filesInput: HTMLInputElement | undefined;
  let cameraInput: HTMLInputElement | undefined;
  let attachBox: HTMLDivElement | undefined;
  // The attach menu closes on Escape and on a press anywhere outside it.
  createEffect(() => {
    if (!attachOpen()) return;
    const onPointer = (e: PointerEvent) => {
      if (attachBox && e.target instanceof Node && !attachBox.contains(e.target)) setAttachOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setAttachOpen(false);
    };
    document.addEventListener("pointerdown", onPointer);
    document.addEventListener("keydown", onKey);
    onCleanup(() => {
      document.removeEventListener("pointerdown", onPointer);
      document.removeEventListener("keydown", onKey);
    });
  });
  const tooLong = createMemo(() => draft().length > CHAT_MAX_CHARS);
  const canSend = createMemo(
    () => (draft().trim().length > 0 || drafts().length > 0 || !!attachLocation()) && !tooLong() && !sending(),
  );
  const visible = createMemo(() => {
    const f = filter();
    // Filter by the derived source while preserving the stored channel.
    return f === "all" ? chat.messages : chat.messages.filter((m) => matchesFilter(m, f));
  });
  // A turn's tool calls fold into one line between the messages.
  const toolRuns = new Map<string, ToolRun>();
  const rows = createMemo(() => chatRows(visible(), toolRuns));
  // Voice notes sent, whose timeline item has not arrived yet (state/voiceNotes.ts).
  const waitingNotes = createMemo(() => {
    const have = new Set(chat.messages.map((m) => m.itemId));
    return stillPending(pendingNotes(), (id) => have.has(id));
  });
  const position = createMemo(() => new Map(visible().map((m, i) => [m.key, i])));
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
  // Deep link from a push or Telegram: /?outreach=po_… (also while already open:
  // a notification click navigates here).
  const location = useOptionalLocation();
  const navigate = useOptionalNavigate();
  const search = () => location?.search ?? (typeof window === "undefined" ? "" : window.location.search);
  const [focusOutreach, setFocusOutreach] = createSignal<string | null>(outreachFromSearch(search()));
  createEffect(
    on(
      search,
      (q) => {
        const id = outreachFromSearch(q);
        if (id) setFocusOutreach(id);
      },
      { defer: true },
    ),
  );
  const focused = createMemo(() => {
    const id = focusOutreach();
    return id ? (chat.messages.find((m) => m.meta?.proactive?.outreachId === id) ?? null) : null;
  });
  /** Drop `?outreach=` from the address, so a reload does not jump again. */
  function clearOutreachParam() {
    if (!search().includes("outreach=")) return;
    if (navigate && location) {
      navigate(location.pathname, { replace: true, scroll: false });
    } else if (typeof window !== "undefined") {
      window.history.replaceState(window.history.state, "", window.location.pathname + window.location.hash);
    }
  }
  function dismissFocus() {
    setFocusOutreach(null);
    clearOutreachParam();
  }
  /** While our own scroll-to-end runs, scroll events are not the reader's. */
  let autoScrollUntil = 0;
  /**
   * When the reader last moved the list (wheel, touch, keys, scrollbar drag). Only their scrolls unpin it: the
   * browser also lowers scrollTop when content above shrinks or the view grows (Secretary Code re-places its warm
   * view on every rail switch), and taking that for a scroll up left the chat stuck above the newest messages.
   */
  let readerInputAt = 0;
  /** Reader input with no scroll event yet: the next one is theirs even when a slow frame delays it. */
  let inputPending = false;
  let touching = false;
  let lastScrollTop = 0;
  let content: HTMLDivElement | undefined;
  let draggingList = false;
  const readerScroll = () => {
    readerInputAt = Date.now();
    inputPending = true;
    autoScrollUntil = 0;
  };

  function autosize() {
    if (!input) return;
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight, 160)}px`;
  }

  function scrollToEnd(smooth: boolean) {
    if (!list || !atBottom) return;
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
    atBottom = false;
    const added = await loadOlder();
    if (added > 0 && list) {
      autoScrollUntil = Date.now() + 100;
      list.scrollTop = top + (list.scrollHeight - height);
      lastScrollTop = list.scrollTop;
    }
  }

  function onScroll() {
    if (!list) return;
    const top = list.scrollTop;
    const movedUp = top < lastScrollTop;
    lastScrollTop = top;
    if (Date.now() < autoScrollUntil) return;
    const distance = list.scrollHeight - top - list.clientHeight;
    const pending = inputPending && Date.now() - readerInputAt < READER_PENDING_MS;
    inputPending = false;
    if (touching || pending || Date.now() - readerInputAt < READER_INPUT_MS) {
      readerInputAt = Date.now();
      // A drop that lands exactly on the end is the browser clamping, not a scroll up.
      atBottom = distance <= 1 || (!movedUp && distance < STICK_PX);
    } else if (atBottom) {
      // Layout moved the list, not the reader: stay on the newest message.
      if (distance > 1) scrollToEnd(false);
      return;
    } else if (distance <= 1) {
      atBottom = true;
    }
    // Older pages load only once the reader has scrolled away from the end, never on mount.
    if (!atBottom && top < LOAD_OLDER_PX && hasMore()) void older();
  }

  /**
   * Pin to the end once the DOM has settled. A hidden page (Secretary Code's view while another rail item is
   * shown, a background tab) runs no animation frames, so it pins at once instead of when it is shown again.
   */
  function pinSoon(smooth: boolean) {
    if (typeof document !== "undefined" && document.visibilityState === "hidden") scrollToEnd(false);
    else requestAnimationFrame(() => scrollToEnd(smooth));
  }

  onMount(() => {
    scrollToEnd(false);
    const frame = requestAnimationFrame(() => scrollToEnd(false));
    // Growing content (photos, previews, fonts, a revealing reply) and a resized view keep a pinned list at the end.
    const observer = typeof ResizeObserver === "function" ? new ResizeObserver(() => {
      if (atBottom) scrollToEnd(false);
    }) : null;
    if (content) observer?.observe(content);
    if (list) observer?.observe(list);
    const latest = () => {
      atBottom = true;
      readerInputAt = 0;
      inputPending = false;
      scrollToEnd(true);
    };
    const release = () => { draggingList = false; };
    const shown = () => {
      if (document.visibilityState === "visible" && atBottom) scrollToEnd(false);
    };
    // Pictures another page handed over ("Ask Lucina about this photo"): attached, the text box ready.
    const handedOver = () => {
      const given = takeHandOff();
      if (!given) return;
      void addFiles(given.files);
      if (given.text) setDraft(given.text);
      input?.focus();
    };
    handedOver();
    window.addEventListener("secretary:chat-handoff", handedOver);
    window.addEventListener("secretary:chat-latest", latest);
    window.addEventListener("pointerup", release);
    window.addEventListener("pointercancel", release);
    document.addEventListener("visibilitychange", shown);
    onCleanup(() => {
      cancelAnimationFrame(frame);
      observer?.disconnect();
      window.removeEventListener("secretary:chat-handoff", handedOver);
      window.removeEventListener("secretary:chat-latest", latest);
      window.removeEventListener("pointerup", release);
      window.removeEventListener("pointercancel", release);
      document.removeEventListener("visibilitychange", shown);
    });
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
        if (atBottom) pinSoon(!firstFill);
      },
      { defer: true },
    ),
  );

  // A revealing reply grows bubble by bubble: stay pinned to the end.
  createEffect(
    on(
      revealTick,
      () => {
        if (atBottom) pinSoon(true);
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

  // Scroll the deep-linked message into view (after the first fill's jump to the end).
  createEffect(
    on(focused, (m) => {
      if (!m) return;
      atBottom = false;
      clearOutreachParam();
      requestAnimationFrame(() => {
        const el = list?.querySelector(`[data-outreach="${m.meta?.proactive?.outreachId}"]`);
        if (el && typeof (el as HTMLElement).scrollIntoView === "function") {
          (el as HTMLElement).scrollIntoView({ block: "center" });
        }
      });
    }),
  );

  createEffect(
    on(
      filter,
      () => {
        atBottom = true;
        pinSoon(false);
      },
      { defer: true },
    ),
  );

  async function addFiles(files: readonly File[]) {
    const list = files.filter((file) => file instanceof File);
    if (!list.length) return;
    setAttachError("");
    try {
      const prepared = await prepareDraftFiles(list);
      setDrafts((all) => [...all, ...prepared]);
    } catch {
      setAttachError(t("share.failed"));
    }
  }

  function removeDraft(item: { id?: string }) {
    const draft = drafts().find((entry) => entry.id === item.id);
    if (!draft) return;
    releaseDraft(draft);
    setDrafts((all) => all.filter((entry) => entry.id !== draft.id));
  }

  function onPaste(e: ClipboardEvent) {
    const files = filesFromClipboard(e.clipboardData);
    // Plain text keeps the browser's default paste.
    if (!files.length) return;
    e.preventDefault();
    void addFiles(files);
  }

  function onDragEnter(e: DragEvent) {
    if (!hasFileTransfer(e.dataTransfer)) return;
    e.preventDefault();
    dragDepth++;
    setDragging(true);
  }

  function onDragOver(e: DragEvent) {
    if (!hasFileTransfer(e.dataTransfer)) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
  }

  function onDragLeave(e: DragEvent) {
    if (!hasFileTransfer(e.dataTransfer)) return;
    e.preventDefault();
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) setDragging(false);
  }

  function onDrop(e: DragEvent) {
    if (!hasFileTransfer(e.dataTransfer)) return;
    e.preventDefault();
    dragDepth = 0;
    setDragging(false);
    void addFiles(filesFromDataTransfer(e.dataTransfer));
  }

  function shareCurrentLocation() {
    setAttachOpen(false);
    setAttachError("");
    const geo = typeof navigator === "undefined" ? undefined : navigator.geolocation;
    if (!geo) {
      setAttachError(t("share.locationFailed"));
      return;
    }
    geo.getCurrentPosition(
      (pos) => setAttachLocation({ lat: pos.coords.latitude, lon: pos.coords.longitude }),
      () => setAttachError(t("share.locationFailed")),
      { enableHighAccuracy: true, timeout: 10_000, maximumAge: 60_000 },
    );
  }

  // "Add secret": `/secret …` opens the sheet instead of sending, and a
  // token-shaped draft asks first (before the optimistic bubble and the cache).
  const [guard, setGuard] = createSignal<{ text: string; spans: Detected[] } | null>(null);
  const [hintProjects, setHintProjects] = createSignal<SecretProject[] | null>(null);
  let hintLoading = false;
  const commandPrefix = createMemo(() => {
    const d = draft().trimStart().toLowerCase();
    return d.startsWith("/") && !/\s/.test(d) && SECRET_COMMAND.startsWith(d) && d !== SECRET_COMMAND;
  });
  const commandArg = createMemo(() => commandArgIndex(draft()));
  const targetHints = createMemo(() => {
    if (commandArg() !== 3) return [];
    const list = hintProjects();
    if (!list) {
      if (!hintLoading) {
        hintLoading = true;
        secretsApi.catalog().then(setHintProjects, () => setHintProjects([]));
      }
      return [];
    }
    const prefix = draft().trimStart().slice(7).trim().split(/\s+/)[2] ?? "";
    return targetCompletions(prefix, list);
  });

  function completeTarget(hint: string) {
    const words = draft().trimStart().slice(7).trim().split(/\s+/);
    setDraft(`${SECRET_COMMAND} ${words[0] ?? ""} ${words[1] ?? ""} ${hint}`);
    input?.focus();
  }

  function guardSave() {
    const g = guard();
    if (!g) return;
    const first = g.spans[0]!;
    const value = g.text.slice(first.start, first.end);
    setDraft(maskSecrets(g.text, g.spans));
    setGuard(null);
    openSecretSheet({ value, name: suggestName(value) || undefined });
  }

  function guardMask() {
    const g = guard();
    if (!g) return;
    setDraft(maskSecrets(g.text, g.spans));
    setGuard(null);
    void submit();
  }

  async function submit() {
    if (!canSend()) return;
    const text = draft();
    if (isSecretCommand(text) && !drafts().length && !attachLocation()) {
      const c = parseSecretCommand(text);
      setDraft("");
      queueMicrotask(autosize);
      openSecretSheet({ name: c.name, value: c.extra ? undefined : c.value, target: c.target });
      return;
    }
    const spans = detectSecrets(text);
    if (spans.length) {
      setGuard({ text, spans });
      return;
    }
    const files = drafts();
    const sharedLocation = attachLocation();
    setDraft("");
    setDrafts([]);
    setAttachLocation(null);
    setAttachOpen(false);
    queueMicrotask(autosize);
    // Your own message must be visible: leave a filter that would hide it.
    if (filter() !== "all" && filter() !== "app") setFilter("all");
    atBottom = true;
    if (files.length || sharedLocation) await sendAttachmentMessage(files, text, sharedLocation);
    else await sendMessage(text);
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
    <div class="chat" onDragEnter={onDragEnter} onDragOver={onDragOver} onDragLeave={onDragLeave} onDrop={onDrop}>
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
        onWheel={readerScroll}
        onTouchStart={() => { touching = true; readerScroll(); }}
        onTouchMove={readerScroll}
        onTouchEnd={() => { touching = false; readerScroll(); }}
        onTouchCancel={() => { touching = false; readerScroll(); }}
        onPointerDown={() => { draggingList = true; }}
        onPointerMove={() => { if (draggingList) readerScroll(); }}
        onKeyDown={(e) => {
          if (["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "].includes(e.key) &&
              !(e.target as Element).closest("input, textarea, [contenteditable=true]")) readerScroll();
        }}
        tabindex="0"
        role="log"
        aria-live="off"
        aria-label={t("chat.conversation")}
      >
        <div class="messages-content" ref={content}>
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
          <Show when={chat.loaded && focusOutreach() && !focused() ? focusOutreach() : null}>
            {(id) => (
              <div class="proactive-missing" role="status">
                <p>{t("chat.outreachMissing")}</p>
                <FeedbackButtons outreachId={id()} />
                <button type="button" class="btn btn--small" onClick={dismissFocus}>
                  {t("chat.close")}
                </button>
              </div>
            )}
          </Show>
          <Show when={chat.messages.length > 0 && visible().length === 0}>
            <p class="timeline-hint">{t("chat.noFiltered", { source: filterLabel() })}</p>
          </Show>
          <For each={rows()}>
            {(row) => (
              <>
                <Show when={dayBreak(position().get(isToolRun(row) ? row.first : row.key) ?? -1)}>
                  {(label) => (
                    <div class="day-sep" role="separator" aria-label={label()}>
                      <span>{label()}</span>
                    </div>
                  )}
                </Show>
                {isToolRun(row) ? (
                  <ToolSteps steps={row.steps} />
                ) : (
                  <MessageItem
                    message={row}
                    focused={!!focusOutreach() && focused()?.key === row.key}
                    inferredChoice={inferredChoices().get(row.key)}
                  />
                )}
              </>
            )}
          </For>
          <For each={waitingNotes()}>
            {(p) => (
              <div class="msg msg--user">
                <div class="bubble bubble--pending-note" role="status">
                  <span aria-hidden="true">🎙</span> {p.transcript ?? t("voiceIn.transcribing")}
                </div>
              </div>
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
        <Show when={guard()}>
          {(g) => (
            <div class="secret-guard" role="alert">
              <p>
                <strong>🔐 {t("secrets.guardTitle", { hint: valueHint(g().text.slice(g().spans[0]!.start, g().spans[0]!.end)) })}</strong>
                <span class="muted small"> {t("secrets.guardSub")}</span>
              </p>
              <div class="secret-guard-actions">
                <button type="button" class="btn btn--small btn--primary" onClick={guardSave}>
                  {t("secrets.guardSave")}
                </button>
                <button type="button" class="btn btn--small" onClick={guardMask}>
                  {t("secrets.guardMask")}
                </button>
                <button type="button" class="btn btn--small" onClick={() => { setGuard(null); input?.focus(); }}>
                  {t("secrets.guardEdit")}
                </button>
              </div>
            </div>
          )}
        </Show>
        <Show when={commandPrefix() || commandArg() > 0}>
          <div class="command-hints" role="listbox" aria-label={t("secrets.attach")}>
            <Show when={commandPrefix()}>
              <button type="button" role="option" class="command-hint" onClick={() => { setDraft(`${SECRET_COMMAND} `); input?.focus(); }}>
                <code>{SECRET_USAGE}</code> <span class="muted small">{t("secrets.commandHint")}</span>
              </button>
            </Show>
            <Show when={commandArg() > 0 && commandArg() < 3}>
              <span class="command-hint"><code>{SECRET_USAGE}</code> <span class="muted small">{t("secrets.commandHint")}</span></span>
            </Show>
            <Show when={commandArg() > 3}>
              <span class="command-hint muted small">{t("secrets.commandExtra")}</span>
            </Show>
            <For each={targetHints()}>
              {(h) => (
                <button type="button" role="option" class="chip" onClick={() => completeTarget(h)}>
                  {h}
                </button>
              )}
            </For>
          </div>
        </Show>
        <Show when={drafts().length || attachLocation() || attachError()}>
          <div class="composer-tray">
            <AttachmentTray items={drafts()} onRemove={removeDraft} />
            <Show when={attachLocation()}>
              {(point) => (
                <div class="composer-location">
                  <span aria-hidden="true">📍</span>
                  <span>{t("share.locationShared")}: {formatCoordinate(point().lat)}, {formatCoordinate(point().lon)}</span>
                  <button
                    type="button"
                    aria-label={t("share.remove", { name: t("share.location") })}
                    onClick={() => setAttachLocation(null)}
                  >
                    ×
                  </button>
                </div>
              )}
            </Show>
            <Show when={attachError()}>
              <p class="field-error" role="alert">{attachError()}</p>
            </Show>
          </div>
        </Show>
        <div class="composer-attach" ref={attachBox}>
          <button
            type="button"
            class="share-attach"
            aria-label={t("share.attachMenu")}
            aria-haspopup="menu"
            aria-expanded={attachOpen() ? "true" : "false"}
            onClick={() => setAttachOpen((open) => !open)}
          >
            📎
          </button>
          <Show when={attachOpen()}>
            <div class="attach-menu" role="menu" aria-label={t("share.attachMenu")}>
              <button type="button" role="menuitem" onClick={() => { setAttachOpen(false); photosInput?.click(); }}>
                <span aria-hidden="true">🖼</span> {t("share.attachPhotos")}
              </button>
              <button type="button" role="menuitem" onClick={() => { setAttachOpen(false); filesInput?.click(); }}>
                <span aria-hidden="true">📄</span> {t("share.attachFiles")}
              </button>
              <button type="button" role="menuitem" onClick={() => { setAttachOpen(false); cameraInput?.click(); }}>
                <span aria-hidden="true">📷</span> {t("share.attachCamera")}
              </button>
              <button type="button" role="menuitem" onClick={shareCurrentLocation}>
                <span aria-hidden="true">📍</span> {t("share.attachLocation")}
              </button>
              <button type="button" role="menuitem" onClick={() => { setAttachOpen(false); openSecretSheet(); }}>
                <span aria-hidden="true">🔐</span> {t("secrets.attach")}
              </button>
            </div>
          </Show>
        </div>
        <Show when={replyTarget()}>{target => <div class="reply-target"><span>{t("chats.replying", { text: target().text })}</span><button type="button" onClick={() => setReplyTarget(null)} aria-label={t("chat.close")}>×</button></div>}</Show>
        <ShareComposer />
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
            setGuard(null);
            autosize();
          }}
          onPaste={onPaste}
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
        <VoiceNoteButton />
        <button class="send-btn" type="submit" disabled={!canSend()} aria-label={t("chat.send")}>
          <SendIcon />
        </button>
      </form>
      <input
        ref={photosInput}
        class="sr-only"
        type="file"
        accept="image/*,video/*"
        multiple
        tabindex="-1"
        aria-label={t("share.attachPhotos")}
        onChange={(e) => {
          const files = Array.from(e.currentTarget.files ?? []);
          e.currentTarget.value = "";
          void addFiles(files);
        }}
      />
      <input
        ref={filesInput}
        class="sr-only"
        type="file"
        multiple
        tabindex="-1"
        aria-label={t("share.attachFiles")}
        onChange={(e) => {
          const files = Array.from(e.currentTarget.files ?? []);
          e.currentTarget.value = "";
          void addFiles(files);
        }}
      />
      <input
        ref={cameraInput}
        class="sr-only"
        type="file"
        accept="image/*"
        capture="environment"
        tabindex="-1"
        aria-label={t("share.attachCamera")}
        onChange={(e) => {
          const files = Array.from(e.currentTarget.files ?? []);
          e.currentTarget.value = "";
          void addFiles(files);
        }}
      />
      <Show when={dragging()}>
        <div class="drop-overlay" role="status">
          <strong>{t("share.dropTitle")}</strong>
          <span>{t("share.dropSub")}</span>
        </div>
      </Show>
    </div>
  );
}
