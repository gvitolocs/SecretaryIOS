import { For, Show, createMemo, createSignal, onCleanup, onMount, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { t } from "../i18n";
import { desktopShell } from "../lib/desktopShell";
import { createMemoRecorder } from "../lib/memoRecorder";
import { connectLive, openMicrophone, type LiveConnection, type LiveEvent, type Microphone } from "../lib/prompterLive";
import { vocabulary, type ScriptSection } from "../lib/prompterScript";
import {
  applyVerdicts,
  candidates,
  currentPoint,
  jumpTo,
  locate,
  newTake,
  nextSection,
  prevSection,
  rebase,
  resetSection,
  toggle,
  type Take,
} from "../lib/prompterTake";

// The live take (docs/prompter.md): full screen, readable from 1-2 m, or the compact
// overlay that Secretary Code keeps above other apps. The microphone streams to
// lucina-memo; verdicts cross points out (lib/prompterTake has the rules).

export interface TakeSettings {
  language: string;
  useJev: boolean;
  alsoRecord: boolean;
  autoAdvance: boolean;
  opacity: number;
}

export interface TakeResult {
  take: Take;
  startedAt: Date;
  durationMs: number;
  finals: { startMs: number; text: string }[];
  jevCalls: number;
  latencies: number[];
  recorded: boolean;
}

type Phase = "starting" | "loading" | "listening" | "paused" | "offline";

export function PrompterLive(props: {
  sections: ScriptSection[];
  settings: TakeSettings;
  overlay?: boolean;
  /** Edit points mid-take: resolves with the new sections, or null when cancelled. */
  onEdit: (sections: ScriptSection[]) => Promise<ScriptSection[] | null>;
  onEnd: (r: TakeResult) => void;
}) {
  const [take, setTake] = createSignal<Take>(newTake(props.sections, { autoAdvance: props.settings.autoAdvance }));
  const [phase, setPhase] = createSignal<Phase>("starting");
  const [engine, setEngine] = createSignal<string>("");
  const [error, setError] = createSignal<string | null>(null);
  const [partial, setPartial] = createSignal("");
  const [finals, setFinals] = createSignal<{ startMs: number; text: string }[]>([]);
  const [latencies, setLatencies] = createSignal<number[]>([]);
  const [jevCalls, setJevCalls] = createSignal(0);
  const [jevDown, setJevDown] = createSignal(false);
  const [level, setLevel] = createSignal(0);
  const [flash, setFlash] = createSignal<string | null>(null);
  const [opacity, setOpacity] = createSignal(props.settings.opacity);
  const startedAt = new Date();
  const t0 = performance.now();
  const elapsed = () => performance.now() - t0;
  const recorder = props.settings.alsoRecord ? createMemoRecorder() : null;
  let conn: LiveConnection | null = null;
  let mic: Microphone | null = null;
  let ended = false;
  let lastPoints = "";
  let listEl: HTMLOListElement | undefined;

  const section = createMemo(() => take().sections[take().current]);
  const sectionsWithPoints = createMemo(() => take().sections.filter((s) => s.points.length));
  const sectionNo = createMemo(() => sectionsWithPoints().indexOf(section()!) + 1);
  const nextTitle = createMemo(
    () =>
      take()
        .sections.slice(take().current + 1)
        .find((s) => s.points.length)?.title ?? null,
  );
  const current = createMemo(() => currentPoint(take()));
  const latency = createMemo(() => {
    const l = latencies()
      .slice(-5)
      .sort((a, b) => a - b);
    return l.length ? l[l.length >> 1]! : null;
  });

  function sendPoints() {
    const tk = take();
    const c = candidates(tk);
    const key = JSON.stringify([tk.current, c.map((x) => x.id)]);
    if (key === lastPoints) return;
    lastPoints = key;
    conn?.points(section()?.title ?? "", c);
  }

  function update(next: Take) {
    setTake(next);
    sendPoints();
    queueMicrotask(() => listEl?.querySelector(".pr-point.is-current")?.scrollIntoView?.({ block: "nearest", behavior: "smooth" }));
  }

  function onEvent(e: LiveEvent) {
    switch (e.type) {
      case "status":
        if (e.state === "ready") {
          setEngine(e.engine ?? "");
          if (phase() !== "paused") setPhase("listening");
        } else setPhase("loading");
        break;
      case "transcript":
        setPartial(e.partial);
        if (e.final) setFinals((f) => [...f, { startMs: e.startMs, text: e.final! }]);
        break;
      case "verdicts": {
        setJevCalls(e.jevCalls);
        if (e.phase === "jev") setJevDown(!!e.jev.error && e.jev.asked > 0);
        const { take: next, ticked } = applyVerdicts(take(), e.verdicts, elapsed());
        if (ticked.length) {
          if (e.latencyMs != null) setLatencies((l) => [...l.slice(-49), e.latencyMs!]);
          setFlash(ticked[ticked.length - 1]!);
          setTimeout(() => setFlash(null), 900);
        }
        if (next !== take()) update(next);
        break;
      }
      case "judge":
        setJevDown(true);
        break;
      case "error":
        if (e.message === "connection") setPhase("offline");
        else setError(e.message);
        break;
      case "closed":
        if (!ended) setPhase("offline");
        break;
    }
  }

  onMount(async () => {
    try {
      mic = await openMicrophone((pcm, lvl) => {
        setLevel(lvl);
        conn?.audio(pcm);
      });
    } catch {
      setError(t("prompter.micError"));
      setPhase("offline");
      return;
    }
    conn = connectLive({
      language: props.settings.language,
      vocabulary: vocabulary(props.sections),
      useJev: props.settings.useJev,
      onEvent,
    });
    sendPoints();
    void recorder?.start();
  });

  function stop() {
    if (ended) return;
    ended = true;
    conn?.close();
    mic?.stop();
    recorder?.stop();
    props.onEnd({
      take: take(),
      startedAt,
      durationMs: elapsed(),
      finals: [...finals(), ...(partial().trim() ? [{ startMs: Number.MAX_SAFE_INTEGER, text: partial().trim() }] : [])],
      jevCalls: jevCalls(),
      latencies: latencies(),
      recorded: !!recorder,
    });
  }

  function togglePause() {
    const p = phase() === "paused";
    conn?.setPaused(!p);
    if (p) recorder?.resume();
    else recorder?.pause();
    setPhase(p ? (engine() ? "listening" : "loading") : "paused");
  }

  async function edit() {
    const wasPaused = phase() === "paused";
    if (!wasPaused) togglePause();
    const next = await props.onEdit(take().sections);
    if (next) {
      lastPoints = "";
      update(rebase(take(), next));
    }
    if (!wasPaused) togglePause();
  }

  const onKey = (e: KeyboardEvent) => {
    const tag = (e.target as HTMLElement | null)?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || e.metaKey || e.ctrlKey || e.altKey) return;
    const k = e.key;
    if (k === " ") togglePause();
    else if (k === "ArrowRight" || k === "n" || k === "N" || k === "PageDown") update(nextSection(take()));
    else if (k === "ArrowLeft" || k === "p" || k === "P" || k === "PageUp") update(prevSection(take()));
    else if (k === "r" || k === "R") update(resetSection(take()));
    else if ((k === "m" || k === "M") && take().missed.length) update(jumpTo(take(), take().missed[0]!));
    else if (k === "e" || k === "E") void edit();
    else if (k === "Escape") stop();
    else if (/^[1-9]$/.test(k)) {
      const p = section()?.points[Number(k) - 1];
      if (p) update(toggle(take(), p.id, elapsed()));
    } else return;
    e.preventDefault();
  };
  window.addEventListener("keydown", onKey);
  onCleanup(() => {
    window.removeEventListener("keydown", onKey);
    if (!ended) {
      ended = true;
      conn?.close();
      mic?.stop();
      recorder?.stop(true);
    }
  });

  const stateLabel = (id: string) => {
    const s = take().state[id];
    return s === "done" ? t("prompter.done") : s === "started" ? t("prompter.started") : t("prompter.waiting");
  };

  const status = (): JSX.Element => (
    <div class="pr-status" role="status">
      <span class={`pr-dot pr-dot--${phase()}`} style={{ "--lvl": String(Math.min(1, level() * 6)) }} aria-hidden="true" />
      <span>
        {phase() === "listening"
          ? t("prompter.listening")
          : phase() === "paused"
            ? t("prompter.paused")
            : phase() === "offline"
              ? t("prompter.connError")
              : t("prompter.loading")}
      </span>
      <Show when={latency() != null}>
        <span class="pr-chip" title={t("prompter.latency")}>
          {t("prompter.latency")} {(latency()! / 1000).toFixed(1)} s
        </span>
      </Show>
      <span class="pr-chip">
        {props.settings.useJev ? (jevDown() ? t("prompter.jevDown") : t("prompter.jevCalls", { count: jevCalls() })) : t("prompter.jevOff")}
      </span>
      <Show when={engine()}>
        <span class="pr-chip">{engine().toUpperCase()}</span>
      </Show>
    </div>
  );

  const shell = desktopShell();

  // Portalled to <body>: the shell's page frame is a size container, which would make it
  // the box of this fixed full-screen layer.
  return (
    <Portal>
      <div class={`pr-live${props.overlay ? " pr-live--overlay" : ""}`} role="dialog" aria-modal={!props.overlay} aria-label={t("prompter.title")}>
        <header class="pr-top">
          <div class="pr-where">
            <p class="pr-kicker">
              {t("prompter.sectionOf", {
                n: sectionNo(),
                count: sectionsWithPoints().length,
              })}
            </p>
            <h1 class="pr-section">{section()?.title || t("prompter.section", { n: sectionNo() })}</h1>
          </div>
          {status()}
        </header>

        <Show when={error()}>
          <p class="pr-error" role="alert">
            {error()}
          </p>
        </Show>

        <ol class="pr-points" ref={listEl}>
          <For each={section()?.points ?? []}>
            {(p, i) => (
              <li
                class={`pr-point is-${take().state[p.id]}`}
                classList={{
                  "is-current": current() === p.id,
                  "is-flash": flash() === p.id,
                }}
                onClick={() => update(toggle(take(), p.id, elapsed()))}
              >
                <span class="pr-tag">
                  <span class="pr-key" aria-hidden="true">
                    {i() + 1}
                  </span>
                  {stateLabel(p.id)}
                </span>
                <span class="pr-text">{p.text}</span>
              </li>
            )}
          </For>
        </ol>

        <footer class="pr-bottom">
          <p class="pr-next">{nextTitle() ? t("prompter.next", { title: nextTitle()! }) : t("prompter.lastSection")}</p>
          <Show when={take().missed.length}>
            <div class="pr-missed" aria-label={t("prompter.missed")}>
              <span class="pr-missed-label" title={t("prompter.missedHint")}>
                {t("prompter.missed")} {take().missed.length}
              </span>
              <For each={take().missed}>
                {(id) => (
                  <button type="button" class="pr-missed-chip" onClick={() => update(jumpTo(take(), id))} title={t("prompter.missedHint")}>
                    {locate(take(), id)?.text}
                  </button>
                )}
              </For>
            </div>
          </Show>
          <p class="pr-heard" aria-live="off">
            <span class="pr-heard-label">{t("prompter.heard")}</span> {(finals().at(-1)?.text ?? "").slice(-90)} <em>{partial()}</em>
          </p>
          <div class="pr-controls">
            <button type="button" class="pr-btn pr-btn--stop" onClick={stop}>
              {t("prompter.stop")}
            </button>
            <button type="button" class="pr-btn" onClick={togglePause}>
              {phase() === "paused" ? t("prompter.resume") : t("prompter.pause")}
            </button>
            <button type="button" class="pr-btn" onClick={() => update(prevSection(take()))} aria-label={t("prompter.prev")}>
              ← <span class="pr-btn-text">{t("prompter.prev")}</span>
            </button>
            <button type="button" class="pr-btn" onClick={() => update(nextSection(take()))} aria-label={t("prompter.nextSection")}>
              <span class="pr-btn-text">{t("prompter.nextSection")}</span> →
            </button>
            <button type="button" class="pr-btn" onClick={() => update(resetSection(take()))}>
              {t("prompter.reset")}
            </button>
            <button type="button" class="pr-btn" onClick={() => void edit()}>
              {t("prompter.edit")}
            </button>
            <Show when={props.overlay && shell?.setOverlayOpacity}>
              <label class="pr-opacity">
                {t("prompter.opacity")}
                <input
                  type="range"
                  min="0.3"
                  max="1"
                  step="0.05"
                  value={opacity()}
                  onInput={(e) => {
                    const v = Number(e.currentTarget.value);
                    setOpacity(v);
                    shell?.setOverlayOpacity?.(v);
                  }}
                />
              </label>
            </Show>
            <span class="pr-shortcuts">{t("prompter.shortcuts")}</span>
          </div>
        </footer>
      </div>
    </Portal>
  );
}
