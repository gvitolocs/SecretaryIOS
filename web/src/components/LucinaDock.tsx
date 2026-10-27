import { useLocation, useNavigate } from "@solidjs/router";
import { createEffect, createMemo, createSignal, on, onCleanup, Show } from "solid-js";
import { t } from "../i18n";
import { createMediaQuery, WIDE_QUERY } from "../lib/media";
import { onLucinaReply, sending, sendMessage } from "../state/chat";
import { embedded } from "../lib/desktopShell";
import { setStageHidden, stageHidden } from "../state/stage";
import { wardrobeOpen } from "../state/wardrobe";
import { LucinaPanel } from "./LucinaPanel";

/**
 * Where Lucina sits:
 * - "side":  desktop (>= 1024 px), a full-height column right of the page, full body.
 * - "strip": mobile on Chat, a compact bust strip above the transcript.
 * - "float": mobile elsewhere, a small avatar button that expands into a card.
 */
export type DockLayout = "side" | "strip" | "float";

const BUBBLE_MIN_MS = 6_000;
const BUBBLE_MAX_MS = 20_000;
const BUBBLE_MS_PER_CHAR = 60;
const BUBBLE_MAX_CHARS = 280;

export function bubbleDuration(text: string): number {
  return Math.min(BUBBLE_MAX_MS, Math.max(BUBBLE_MIN_MS, text.length * BUBBLE_MS_PER_CHAR));
}

function clip(text: string): string {
  return text.length > BUBBLE_MAX_CHARS ? `${text.slice(0, BUBBLE_MAX_CHARS - 1).trimEnd()}…` : text;
}

/**
 * The one Lucina stage of the session. The shell mounts this once, so the
 * stage (and its WebGL context) survives navigation; only the layout changes.
 */
export function LucinaDock() {
  const location = useLocation();
  const navigate = useNavigate();
  const wide = createMediaQuery(WIDE_QUERY);
  const onChat = createMemo(() => location.pathname === "/");
  // Inside Secretary Code she is on stage only in the chat (and All chats); plugin pages get the whole window.
  // Hidden, not unmounted: the stage and its WebGL context survive, like the float layout.
  const inShell = embedded();
  const offstage = createMemo(
    () => inShell && !wardrobeOpen() && location.pathname !== "/" && !location.pathname.startsWith("/chats"),
  );
  const layout = createMemo<DockLayout>(() => (wide() ? "side" : onChat() ? "strip" : "float"));
  const [expanded, setExpanded] = createSignal(false);
  const [bubble, setBubble] = createSignal<string | null>(null);
  let bubbleTimer: ReturnType<typeof setTimeout> | undefined;

  // The float card closes again when you navigate.
  createEffect(on(() => location.pathname, () => setExpanded(false), { defer: true }));

  function dismissBubble() {
    clearTimeout(bubbleTimer);
    setBubble(null);
  }

  // Away from Chat, her reply appears next to her (it is in the timeline too).
  onCleanup(
    onLucinaReply((text) => {
      if (onChat() || stageHidden() || offstage()) return;
      clearTimeout(bubbleTimer);
      setBubble(clip(text));
      bubbleTimer = setTimeout(() => setBubble(null), bubbleDuration(text));
    }),
  );
  onCleanup(() => clearTimeout(bubbleTimer));
  createEffect(on(onChat, (chat) => chat && dismissBubble(), { defer: true }));

  function typeInstead() {
    const focus = () => document.getElementById("composer-input")?.focus();
    if (onChat()) {
      focus();
      return;
    }
    navigate("/");
    requestAnimationFrame(focus);
  }

  // The wardrobe needs her on stage (the live preview), even when the stage was hidden.
  const stageVisible = () => layout() !== "float" || expanded() || wardrobeOpen();

  return (
    <Show when={!stageHidden() || wardrobeOpen()}>
      <aside
        class="dock"
        classList={{
          "dock--side": layout() === "side",
          "dock--strip": layout() === "strip",
          "dock--float": layout() === "float",
          "dock--open": layout() === "float" && expanded(),
          "dock--wardrobe": wardrobeOpen(),
          "dock--offstage": offstage(),
        }}
        hidden={offstage()}
        data-layout={layout()}
        aria-label={t("shell.lucina")}
      >
        {/* Always mounted while shown; the float button only hides it (one WebGL context). */}
        <div class="dock-stage" hidden={!stageVisible()}>
          <LucinaPanel
            framing={layout() === "side" || wardrobeOpen() ? "full" : "bust"}
            thinking={sending()}
            onTranscript={(text) => void sendMessage(text)}
            onTypeInstead={typeInstead}
            onCollapse={() => {
              dismissBubble();
              setStageHidden(true);
            }}
            onMinimize={layout() === "float" ? () => setExpanded(false) : undefined}
          />
        </div>
        <Show when={layout() === "float" && !expanded() && !wardrobeOpen()}>
          <button
            type="button"
            class="dock-fab"
            classList={{ "dock-fab--thinking": sending() }}
            onClick={() => setExpanded(true)}
            aria-label={t("shell.lucinaOpen")}
            aria-expanded="false"
          >
            <img src="/icon-192.png" alt="" width="56" height="56" />
          </button>
        </Show>
        <Show when={!wardrobeOpen() && bubble()}>
          {(text) => (
            <div class="dock-bubble" role="status" onClick={dismissBubble}>
              <span class="sr-only">{t("shell.lucinaSays")} </span>
              <span class="dock-bubble-text">{text()}</span>
              <button type="button" class="dock-bubble-close" onClick={dismissBubble} aria-label={t("common.dismiss")}>
                ×
              </button>
            </div>
          )}
        </Show>
      </aside>
    </Show>
  );
}
