import { useLocation, useNavigate } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show, type JSX } from "solid-js";
import type { Exercise, ExerciseKind, GymCommand, GymView, SetKind, WorkoutSet } from "../../api/gym";
import { formatDistance, formatDuration } from "../../lib/units";
import { ErrorState, LoadingState, Page, PageHeader, PrimaryAction, SectionNav, useWide } from "../../components/page";
import { dismissGymError, elapsedNow, gym, refreshGym, restRemainingNow, runGym, tickClock } from "../../state/gym";
import "../../styles/gym.css";
import { GearIcon, PlayIcon } from "./icons";
import { gt, gymLocale, type GymKey } from "./strings";

// Shared pieces for the Gym screens. Everything computed (volume, records,
// labels, rest time) comes from the server view; these helpers only format.

let lastRefreshAt = 0;

/** Refetch the gym view unless a fresh copy arrived within `maxAgeMs`. */
export function refreshGymIfStale(maxAgeMs: number): void {
  if (gym.loaded && Date.now() - lastRefreshAt < maxAgeMs) return;
  lastRefreshAt = Date.now();
  void refreshGym();
}

const KIND_KEYS: Record<ExerciseKind, GymKey> = {
  weight_reps: "kindWeightReps",
  reps_only: "kindRepsOnly",
  timed: "kindTimed",
  distance_time: "kindDistanceTime",
};

export const EXERCISE_KINDS = Object.keys(KIND_KEYS) as ExerciseKind[];

export function kindLabel(kind: ExerciseKind): string {
  return gt(KIND_KEYS[kind] ?? "kindWeightReps");
}

const SET_KIND_KEYS: Record<SetKind, GymKey> = {
  normal: "setNormal",
  warmup: "setWarmup",
  drop: "setDrop",
  failure: "setFailure",
};

export const SET_KINDS = Object.keys(SET_KIND_KEYS) as SetKind[];

export function setKindLabel(kind: SetKind): string {
  return gt(SET_KIND_KEYS[kind] ?? "setNormal");
}

/** Badge text for a set: its number for normal sets, W/D/F otherwise. */
export function setBadge(kind: SetKind, index: number): string {
  if (kind === "normal") return String(index + 1);
  return kind.charAt(0).toUpperCase();
}

/** "1 set", "3 sets". */
export function setsText(n: number): string {
  return gt("sets", { count: n });
}

export function exerciseById(view: GymView, id: string): Exercise | undefined {
  return view.state.exercises.find((e) => e.id === id);
}

/** Human text for one set, using the server's weight label. */
export function setValueText(view: GymView, set: WorkoutSet, kind: ExerciseKind): string {
  switch (kind) {
    case "reps_only":
      return `${set.reps} reps`;
    case "timed":
      return formatDuration(set.seconds);
    case "distance_time":
      return `${formatDistance(set.distance_meters)} · ${formatDuration(set.seconds)}`;
    default:
      return `${view.weight_labels[set.id] ?? "0"} ${view.state.settings.weight_unit} × ${set.reps}`;
  }
}

// Settings is a Gym section of its own. The app router (src/index.tsx) has no
// /gym/settings route, so the Training route shows it for ?tab=settings.
export const SETTINGS_HREF = "/gym?tab=settings";
export const JOURNAL_HREF = "/gym/history";

export function isSettingsSearch(search: string): boolean {
  return new URLSearchParams(search).get("tab") === "settings";
}

type GymTab = "training" | "journal" | "progress" | "settings";

function currentTab(pathname: string, search: string): GymTab | null {
  const path = pathname.replace(/\/+$/, "") || "/";
  if (path === "/gym" || path === "/training") return isSettingsSearch(search) ? "settings" : "training";
  if (path.startsWith("/gym/history")) return "journal";
  if (path.startsWith("/gym/progress")) return "progress";
  return null;
}

const TABS: { tab: GymTab; href: string; label: GymKey }[] = [
  { tab: "training", href: "/gym", label: "tabTraining" },
  { tab: "journal", href: JOURNAL_HREF, label: "tabJournal" },
  { tab: "progress", href: "/gym/progress", label: "tabProgress" },
  { tab: "settings", href: SETTINGS_HREF, label: "tabSettings" },
];

/** Training / Journal / Progress / Settings, the original app's four tabs. */
export function GymSubNav() {
  const location = useLocation();
  const active = createMemo(() => currentTab(location.pathname, location.search));
  return (
    <nav class="gym-subnav" aria-label={gt("sections")}>
      <For each={TABS}>
        {(t) => (
          // A plain link (the router still handles the click): <A> would mark
          // Training current on /gym?tab=settings, since it ignores the query.
          <a
            href={t.href}
            class="gym-subnav-tab"
            classList={{ "gym-subnav-tab--active": active() === t.tab }}
            aria-current={active() === t.tab ? "page" : undefined}
          >
            {gt(t.label)}
          </a>
        )}
      </For>
    </nav>
  );
}

/** Uppercase, tracked label (Components.swift `Eyebrow`). */
export function Eyebrow(props: { children: JSX.Element; class?: string }) {
  return <p class={`eyebrow gym-eyebrow ${props.class ?? ""}`}>{props.children}</p>;
}

/** Value over an eyebrow label (Components.swift `Metric`). */
export function Metric(props: { value: JSX.Element; label: string }) {
  return (
    <div class="gym-metric">
      <span class="gym-metric-value">{props.value}</span>
      <span class="gym-eyebrow gym-metric-label">{props.label}</span>
    </div>
  );
}

/** Icon, title and detail on a card (Components.swift `EmptyCard`). */
export function EmptyCard(props: { icon: JSX.Element; title: string; detail: string }) {
  return (
    <div class="card gym-empty-card">
      <span class="gym-empty-icon">{props.icon}</span>
      <p class="gym-empty-title">{props.title}</p>
      <p class="gym-empty-detail">{props.detail}</p>
    </div>
  );
}

/**
 * Rest countdown and live workout duration. The server's clock is the source
 * of truth (`rest_remaining`, `active_elapsed_seconds`); between updates the
 * display ticks locally once a second, and only while something is running.
 */
export function useGymClock() {
  const [now, setNow] = createSignal(Date.now());

  // A fresh clock from the server resets the local tick base.
  createEffect(on(() => gym.clock?.receivedAt, () => setNow(Date.now())));

  const restRemaining = createMemo(() => {
    const c = gym.clock;
    if (!c) return 0;
    return restRemainingNow(c, Math.max(now(), c.receivedAt));
  });

  const elapsed = createMemo(() => {
    const c = gym.clock;
    if (!c || !gym.view?.state.active) return null;
    return elapsedNow(c, Math.max(now(), c.receivedAt));
  });

  const ticking = createMemo(() => restRemaining() > 0 || elapsed() !== null);

  createEffect(() => {
    if (!ticking()) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    onCleanup(() => clearInterval(timer));
  });

  // When the local countdown runs out, ask the server once for the real clock.
  createEffect(
    on(restRemaining, (left, prev) => {
      if (left === 0 && prev !== undefined && prev > 0) void tickClock();
    }),
  );

  const duration = () => {
    const e = elapsed();
    if (e !== null) return formatDuration(e);
    return gym.clock?.active_duration ?? gym.view?.summary.active_duration ?? "";
  };

  return { restRemaining, duration };
}

/** Overview · Journal · Progress · settings: the Gym sections in the page header. */
export function GymSectionNav() {
  const location = useLocation();
  const tab = createMemo(() => currentTab(location.pathname, location.search));
  return (
    <SectionNav
      label={gt("sections")}
      items={[
        { href: "/gym", label: gt("tabOverview"), current: tab() === "training" },
        { href: JOURNAL_HREF, label: gt("tabJournal"), current: tab() === "journal" },
        { href: "/gym/progress", label: gt("tabProgress"), current: tab() === "progress" },
        { href: SETTINGS_HREF, label: gt("tabSettings"), current: tab() === "settings", icon: <GearIcon /> },
      ]}
    />
  );
}

/**
 * Start (or repeat) a workout and open it; `starting` is true while the
 * command runs, so a double click starts one workout.
 */
export function useStartWorkout() {
  const navigate = useNavigate();
  const [starting, setStarting] = createSignal(false);
  async function start(command: GymCommand) {
    if (starting()) return;
    setStarting(true);
    try {
      const res = await runGym(command);
      if (res?.ok) navigate("/gym/workout");
    } finally {
      setStarting(false);
    }
  }
  return { starting, start };
}

/**
 * The desktop header of every Gym section: Training, today's date, the
 * sections and the one accent button (start a workout, or continue the one in
 * progress). It stays put while the sections switch the content below it.
 */
function GymDeskHeader() {
  const { starting, start } = useStartWorkout();
  const [now, setNow] = createSignal(Date.now());
  onMount(() => {
    const timer = setInterval(() => setNow(Date.now()), 60_000);
    onCleanup(() => clearInterval(timer));
  });
  const today = () =>
    new Intl.DateTimeFormat(gymLocale(), { weekday: "long", day: "numeric", month: "long" }).format(new Date(now()));

  return (
    <PageHeader
      title={gt("tabTraining")}
      context={today()}
      nav={<GymSectionNav />}
      action={
        <Show when={gym.view}>
          {(view) => (
            <Show
              when={view().state.active}
              fallback={
                <PrimaryAction
                  icon={<PlayIcon />}
                  disabled={starting()}
                  onClick={() => void start({ type: "start_workout", title: gt("defaultWorkoutTitle") })}
                >
                  {gt("startWorkout")}
                </PrimaryAction>
              }
            >
              <PrimaryAction href="/gym/workout" icon={<PlayIcon />}>
                {gt("continueWorkout")}
              </PrimaryAction>
            </Show>
          )}
        </Show>
      }
    />
  );
}

/**
 * Page frame for every Gym route: loads the view, refreshes it on focus,
 * and renders the shared loading / error / notice states. On a page at least
 * WIDE_MIN_PX wide it is the page system's frame (components/page) with the
 * Gym header row; on a phone the Lucina Gym theme with the tab bar
 * (src/styles/gym.css).
 */
export function GymPage(props: {
  subnav?: boolean;
  /** Force the desktop frame (tests); otherwise the page measures its own width. */
  wide?: boolean;
  /** Told whenever the page switches between its phone and desktop layouts. */
  onWide?: (wide: boolean) => void;
  children: (view: () => GymView, wide: () => boolean) => JSX.Element;
}) {
  const measured = useWide();
  const wide = () => props.wide ?? measured.wide();
  createEffect(() => props.onWide?.(wide()));

  const body = () => (
    <>
      <Show when={gym.actionError}>
        {(msg) => (
          <div class={wide() ? "pg-notice" : "notice"} role="alert">
            <span>{msg()}</span>
            <button type="button" class="banner-close" onClick={dismissGymError} aria-label={gt("dismiss")}>
              ×
            </button>
          </div>
        )}
      </Show>
      <Show
        when={gym.loaded ? gym.view : null}
        fallback={
          <Show
            when={gym.loadError}
            fallback={
              <Show when={wide()} fallback={<p class="muted center" role="status">{gt("loading")}</p>}>
                <LoadingState label={gt("loading")} />
              </Show>
            }
          >
            {(msg) => (
              <Show
                when={wide()}
                fallback={
                  <div class="empty" role="alert">
                    <p class="empty-title">{msg()}</p>
                    <button type="button" class="btn" onClick={() => void refreshGym()}>
                      {gt("tryAgain")}
                    </button>
                  </div>
                }
              >
                <ErrorState message={msg()} retryLabel={gt("tryAgain")} onRetry={() => void refreshGym()} />
              </Show>
            )}
          </Show>
        }
      >
        {(view) => (
          <>
            <Show when={gym.loadError}>
              {(msg) => (
                <p class={wide() ? "pg-context" : "muted small"} role="status">
                  {gt("staleCopy", { message: msg() })}
                </p>
              )}
            </Show>
            {props.children(view, wide)}
          </>
        )}
      </Show>
    </>
  );

  onMount(() => {
    refreshGymIfStale(5_000);
    const onFocus = () => refreshGymIfStale(2_000);
    const onVisible = () => {
      if (document.visibilityState === "visible") onFocus();
    };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisible);
    onCleanup(() => {
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisible);
    });
  });

  return (
    <div class="gym-page gym-theme" classList={{ "gym-page--wide": wide() }} ref={measured.track}>
      <Show
        when={wide()}
        fallback={
          <div class="gym-inner">
            <Show when={props.subnav !== false}>
              <GymSubNav />
            </Show>
            {body()}
          </div>
        }
      >
        <Page class="gym-desk">
          <GymDeskHeader />
          {body()}
        </Page>
      </Show>
    </div>
  );
}

// The desktop switch is the page system's (components/page/useWide.ts).
export { useWide, WIDE_MIN_PX } from "../../components/page";

/**
 * Two-step destructive button: the first click arms it, the second runs
 * `onConfirm`. Disarms on blur-free Cancel.
 */
export function ConfirmButton(props: {
  label: string;
  confirmLabel: string;
  onConfirm: () => Promise<unknown> | void;
  class?: string;
  disabled?: boolean;
  ariaLabel?: string;
  /** Question shown next to the confirm buttons once armed. */
  prompt?: string;
}) {
  const [armed, setArmed] = createSignal(false);
  const [pending, setPending] = createSignal(false);

  async function confirm() {
    if (pending()) return;
    setPending(true);
    try {
      await props.onConfirm();
    } finally {
      setPending(false);
      setArmed(false);
    }
  }

  return (
    <Show
      when={armed()}
      fallback={
        <button
          type="button"
          class={`btn btn--small ${props.class ?? ""}`}
          disabled={props.disabled}
          aria-label={props.ariaLabel}
          onClick={() => setArmed(true)}
        >
          {props.label}
        </button>
      }
    >
      <span class="confirm-pair" role="group" aria-label={props.prompt ?? props.ariaLabel ?? props.label}>
        <Show when={props.prompt}>{(p) => <span class="confirm-prompt">{p()}</span>}</Show>
        <button type="button" class="btn btn--small btn--danger" disabled={pending()} onClick={() => void confirm()}>
          {props.confirmLabel}
        </button>
        <button type="button" class="btn btn--small" disabled={pending()} onClick={() => setArmed(false)}>
          {gt("cancel")}
        </button>
      </span>
    </Show>
  );
}
