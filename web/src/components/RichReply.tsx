import { openSecretSheet } from "../state/secretSheet";
import { tidyDecisionText } from "../lib/decisionText";
import { useNavigate } from "@solidjs/router";
import { createMemo, createSignal, For, Match, onCleanup, onMount, Show, Switch } from "solid-js";
import { t } from "../i18n";
import { LIST_VISIBLE, typingDelay, type RichAction, type RichBlock, type RichMessage } from "../lib/rich";
import {
  bumpReveal,
  chooseAction,
  chosenAction,
  requestPrefill,
  sending,
  sendMessage,
  type ChatMessage,
} from "../state/chat";
import { InlineMarkdown, Markdown } from "./Markdown";
import { PlaceCard } from "./PlaceCard";

// Lucina message format v1 in the web chat (docs/lucina-message-format.md):
// bubbles one by one with a typing pause, then widgets, then buttons. All text
// goes through the Notes markdown renderer: no innerHTML, safe links only.

function prefersReducedMotion(): boolean {
  return typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
}

function Title(props: { text?: string }) {
  return (
    <Show when={props.text}>
      <h4 class="rw-title">{props.text}</h4>
    </Show>
  );
}

function ListWidget(props: { block: RichBlock & { type: "list" } }) {
  const [open, setOpen] = createSignal(false);
  const items = () => (open() ? props.block.items : props.block.items.slice(0, LIST_VISIBLE));
  const hidden = () => props.block.items.length - LIST_VISIBLE;
  return (
    <section class="rw rw-list">
      <Title text={props.block.title} />
      <ul>
        <For each={items()}>
          {(item) => (
            <li>
              <span class="rw-bullet" aria-hidden="true">
                {item.emoji ?? "•"}
              </span>
              <span class="rw-text">
                <InlineMarkdown text={item.text} />
                <Show when={item.meta}>
                  <span class="rw-meta">{item.meta}</span>
                </Show>
              </span>
            </li>
          )}
        </For>
      </ul>
      <Show when={hidden() > 0}>
        <button type="button" class="rw-more" aria-expanded={open() ? "true" : "false"} onClick={() => setOpen((v) => !v)}>
          {open() ? t("chat.richShowLess") : t("chat.richShowMore", { count: hidden() })}
        </button>
      </Show>
    </section>
  );
}

/** A picture from this server (a map PNG): same-origin, so the session cookie loads it; a tap opens it full size. */
function ImageWidget(props: { block: RichBlock & { type: "image" } }) {
  const [failed, setFailed] = createSignal(false);
  return (
    <figure class="rw rw-image">
      <Show when={!failed()} fallback={<p class="rw-image-error">{t("chat.richImageUnavailable")}</p>}>
        <a href={props.block.src} target="_blank" rel="noopener noreferrer" aria-label={t("chat.richImageOpen", { title: props.block.alt })}>
          <img
            src={props.block.src}
            alt={props.block.alt}
            width={props.block.width}
            height={props.block.height}
            loading="lazy"
            decoding="async"
            onError={() => setFailed(true)}
          />
        </a>
      </Show>
      <Show when={props.block.caption}>
        <figcaption>{props.block.caption}</figcaption>
      </Show>
    </figure>
  );
}

function Widget(props: { block: RichBlock }) {
  return (
    <Switch>
      <Match when={props.block.type === "checklist" && props.block}>
        {(b) => (
          <section class="rw rw-checklist">
            <Title text={b().title} />
            <ul>
              <For each={b().items}>
                {(item) => (
                  <li classList={{ "is-done": !!item.done }}>
                    <span class="rw-check" aria-hidden="true" />
                    <span class="sr-only">{item.done ? t("chat.richDone") : t("chat.richTodo")}: </span>
                    <span class="rw-text">
                      <InlineMarkdown text={item.text} />
                    </span>
                  </li>
                )}
              </For>
            </ul>
          </section>
        )}
      </Match>
      <Match when={props.block.type === "card" && props.block}>
        {(b) => (
          <section class="rw rw-card" data-tone={b().tone}>
            <div class="rw-card-head">
              <Show when={b().emoji}>
                <span class="rw-emoji" aria-hidden="true">
                  {b().emoji}
                </span>
              </Show>
              <div>
                <h4 class="rw-card-title">{b().title}</h4>
                <Show when={b().subtitle}>
                  <p class="rw-sub">{b().subtitle}</p>
                </Show>
              </div>
            </div>
            <Show when={b().body}>{(body) => <Markdown text={body()} class="rw-body" />}</Show>
          </section>
        )}
      </Match>
      <Match when={props.block.type === "stat" && props.block}>
        {(b) => (
          <div class="rw rw-stats" role="group">
            <For each={b().items}>
              {(s) => (
                <div class="rw-stat">
                  <span class="rw-stat-label">{s.label}</span>
                  <span class="rw-stat-value">{s.value}</span>
                  <Show when={s.delta}>
                    {(d) => (
                      <span class="rw-stat-delta" data-dir={/^[-−–]/.test(d()) ? "down" : /^\+/.test(d()) ? "up" : undefined}>
                        {d()}
                      </span>
                    )}
                  </Show>
                </div>
              )}
            </For>
          </div>
        )}
      </Match>
      <Match when={props.block.type === "list" && props.block}>{(b) => <ListWidget block={b()} />}</Match>
      <Match when={props.block.type === "table" && props.block}>
        {(b) => (
          <section class="rw rw-tablebox">
            <Title text={b().title} />
            <div class="rw-table-wrap">
              <table class="rw-table">
                <thead>
                  <tr>
                    <For each={b().columns}>{(c) => <th scope="col">{c}</th>}</For>
                  </tr>
                </thead>
                <tbody>
                  <For each={b().rows}>
                    {(row) => (
                      <tr>
                        <For each={row}>{(cell) => <td>{cell}</td>}</For>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
          </section>
        )}
      </Match>
      <Match when={props.block.type === "steps" && props.block}>
        {(b) => (
          <section class="rw rw-steps">
            <Title text={b().title} />
            <ol>
              <For each={b().items}>
                {(item) => (
                  <li>
                    <InlineMarkdown text={item} />
                  </li>
                )}
              </For>
            </ol>
          </section>
        )}
      </Match>
      <Match when={props.block.type === "quote" && props.block}>
        {(b) => (
          <figure class="rw rw-quote">
            <blockquote>
              <Markdown text={b().text} />
            </blockquote>
            <Show when={b().cite}>
              <figcaption>— {b().cite}</figcaption>
            </Show>
          </figure>
        )}
      </Match>
      <Match when={props.block.type === "place" && props.block}>
        {(b) => <PlaceCard place={b()} class="rw-place" />}
      </Match>
      <Match when={props.block.type === "link_preview" && props.block}>
        {(b) => (
          <a
            class="rw rw-link"
            href={b().url}
            target="_blank"
            rel="noopener noreferrer nofollow"
            aria-label={t("chat.richLink", { title: b().title })}
          >
            <span class="rw-link-icon" aria-hidden="true">
              🔗
            </span>
            <span class="rw-link-text">
              <span class="rw-link-title">{b().title}</span>
              <span class="rw-link-host">{new URL(b().url).hostname}</span>
            </span>
          </a>
        )}
      </Match>
      <Match when={props.block.type === "image" && props.block}>{(b) => <ImageWidget block={b()} />}</Match>
    </Switch>
  );
}

function useAppNavigate(): (path: string) => void {
  try {
    return useNavigate();
  } catch {
    // Rendered outside the router (tests): a plain navigation.
    return (path) => window.location.assign(path);
  }
}

/** The button sends a message right away (not a prefill, route or dismiss). */
function sendsMessage(a: RichAction): boolean {
  if (a.kind === "reply") return !a.payload.prefill;
  if (a.kind === "confirm") return a.payload.answer !== "edit";
  return false;
}

function Actions(props: { message: ChatMessage; actions: RichAction[]; inferred?: string; readOnly?: boolean }) {
  const navigate = useAppNavigate();
  const chosen = () => chosenAction(props.message) ?? props.inferred;

  function run(a: RichAction) {
    if (props.readOnly || chosen() || (sending() && sendsMessage(a))) return;
    switch (a.kind) {
      case "reply":
        if (a.payload.prefill) requestPrefill(a.payload.text);
        else void sendMessage(a.payload.text);
        break;
      case "confirm":
        if (a.payload.answer === "edit") requestPrefill(a.payload.text ?? "");
        else if (a.payload.text) void sendMessage(a.payload.text);
        break;
      case "route":
        navigate(a.payload.path);
        break;
      case "dismiss":
        break;
      case "secret":
        // Names only; the sheet asks for the value (or sends the held id).
        openSecretSheet(a.payload);
        break;
    }
    chooseAction(props.message, a.id);
  }

  return (
    <div class="rich-actions" role="group" aria-label={t("chat.richActions")}>
      <For each={props.actions}>
        {(a) => {
          const isChosen = () => chosen() === a.id;
          return (
            <button
              type="button"
              class="rich-action"
              classList={{ "rich-action--chosen": isChosen(), [`rich-action--${a.kind}`]: true }}
              data-answer={a.kind === "confirm" ? a.payload.answer : undefined}
              disabled={props.readOnly || !!chosen() || (sending() && sendsMessage(a))}
              aria-pressed={isChosen() ? "true" : undefined}
              aria-label={isChosen() ? t("chat.richChosen", { label: a.label }) : undefined}
              onClick={() => run(a)}
            >
              <Show when={isChosen()} fallback={<Show when={a.emoji}>{(e) => <span aria-hidden="true">{e()}</span>}</Show>}>
                <span class="rich-action-check" aria-hidden="true">
                  ✓
                </span>
              </Show>
              <span>{a.label}</span>
            </button>
          );
        }}
      </For>
    </div>
  );
}

/**
 * One Lucina reply in format v1. `onBubbleClick` toggles the time like a plain
 * bubble; `inferredChoice` is a button the owner already answered by typing.
 */
export function RichReply(props: {
  message: ChatMessage;
  rich: RichMessage;
  onBubbleClick?: (e: MouseEvent) => void;
  inferredChoice?: string;
  readOnly?: boolean;
}) {
  const total = () => props.rich.bubbles.length;
  const animate = !!props.message.reveal && !prefersReducedMotion() && total() > 1;
  const [shown, setShown] = createSignal(animate ? 1 : total());
  const [typing, setTyping] = createSignal(false);
  const done = createMemo(() => shown() >= total());
  const timers: ReturnType<typeof setTimeout>[] = [];

  onMount(() => {
    if (!animate) return;
    let at = 0;
    for (let i = 1; i < total(); i++) {
      const gap = 160;
      timers.push(setTimeout(() => {
        setTyping(true);
        bumpReveal();
      }, at + gap));
      at += gap + typingDelay(props.rich.bubbles[i] ?? "");
      timers.push(setTimeout(() => {
        setTyping(false);
        setShown(i + 1);
        bumpReveal();
      }, at));
    }
  });
  onCleanup(() => timers.forEach(clearTimeout));

  return (
    <div class="rich-reply" lang={props.rich.lang}>
      <For each={props.rich.bubbles.slice(0, shown())}>
        {(b) => (
          <div class="bubble bubble--rich" onClick={(e) => props.onBubbleClick?.(e)}>
            <Markdown text={tidyDecisionText(b)} class="md--chat" />
          </div>
        )}
      </For>
      <Show when={typing()}>
        <div class="bubble bubble--typing" role="status" aria-label={t("chat.richTyping")}>
          <span class="typing" aria-hidden="true">
            <span />
            <span />
            <span />
          </span>
        </div>
      </Show>
      <Show when={done()}>
        <Show when={props.rich.blocks.length}>
          <div class="rich-blocks" classList={{ "rich-in": animate }}>
            <For each={props.rich.blocks}>{(b) => <Widget block={b} />}</For>
          </div>
        </Show>
        <Show when={props.rich.actions.length}>
          <Actions readOnly={props.readOnly} message={props.message} actions={props.rich.actions} inferred={props.inferredChoice} />
        </Show>
      </Show>
    </div>
  );
}
