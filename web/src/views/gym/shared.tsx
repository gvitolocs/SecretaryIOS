import { A } from "@solidjs/router";
import { createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";
import type { Exercise, ExerciseKind, GymView, SetKind, WorkoutSet } from "../../api/gym";
import { formatDistance, formatDuration } from "../../lib/units";
import { dismissGymError, gym, refreshGym } from "../../state/gym";

// Shared pieces for the Gym screens. Everything computed (volume, records,
// labels, rest time) comes from the server view; these helpers only format.

let lastRefreshAt = 0;

/** Refetch the gym view unless a fresh copy arrived within `maxAgeMs`. */
export function refreshGymIfStale(maxAgeMs: number): void {
  if (gym.loaded && Date.now() - lastRefreshAt < maxAgeMs) return;
  lastRefreshAt = Date.now();
  void refreshGym();
}

export const KIND_LABELS: Record<ExerciseKind, string> = {
  weight_reps: "Weight & reps",
  reps_only: "Reps only",
  timed: "Time",
  distance_time: "Distance & time",
};

export const SET_KIND_LABELS: Record<SetKind, string> = {
  normal: "Normal",
  warmup: "Warmup",
  drop: "Drop",
  failure: "Failure",
};

/** Badge text for a set: its number for normal sets, W/D/F otherwise. */
export function setBadge(kind: SetKind, index: number): string {
  if (kind === "normal") return String(index + 1);
  return kind.charAt(0).toUpperCase();
}

/** "1 set", "3 sets". */
export function setsText(n: number): string {
  return n === 1 ? "1 set" : `${n} sets`;
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

export function GymSubNav() {
  return (
    <nav class="gym-subnav" aria-label="Gym sections">
      <A href="/gym" end class="gym-subnav-tab" activeClass="gym-subnav-tab--active">
        Train
      </A>
      <A href="/gym/history" class="gym-subnav-tab" activeClass="gym-subnav-tab--active">
        History
      </A>
      <A href="/gym/progress" class="gym-subnav-tab" activeClass="gym-subnav-tab--active">
        Progress
      </A>
    </nav>
  );
}

/**
 * Page frame for every Gym route: loads the view, refreshes it on focus,
 * and renders the shared loading / error / notice states.
 */
export function GymPage(props: {
  subnav?: boolean;
  children: (view: () => GymView) => JSX.Element;
}) {
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
    <div class="gym-page">
      <div class="gym-inner">
        <Show when={props.subnav !== false}>
          <GymSubNav />
        </Show>
        <Show when={gym.actionError}>
          {(msg) => (
            <div class="notice" role="alert">
              <span>{msg()}</span>
              <button type="button" class="banner-close" onClick={dismissGymError} aria-label="Dismiss">
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
                <p class="muted center" role="status">
                  Loading your training…
                </p>
              }
            >
              {(msg) => (
                <div class="empty" role="alert">
                  <p class="empty-title">{msg()}</p>
                  <button type="button" class="btn" onClick={() => void refreshGym()}>
                    Try again
                  </button>
                </div>
              )}
            </Show>
          }
        >
          {(view) => (
            <>
              <Show when={gym.loadError}>
                {(msg) => (
                  <p class="muted small" role="status">
                    {msg()} Showing the last copy.
                  </p>
                )}
              </Show>
              {props.children(view)}
            </>
          )}
        </Show>
      </div>
    </div>
  );
}

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
          onClick={() => setArmed(true)}
        >
          {props.label}
        </button>
      }
    >
      <span class="confirm-pair" role="group" aria-label={props.label}>
        <button type="button" class="btn btn--small btn--danger" disabled={pending()} onClick={() => void confirm()}>
          {props.confirmLabel}
        </button>
        <button type="button" class="btn btn--small" disabled={pending()} onClick={() => setArmed(false)}>
          Cancel
        </button>
      </span>
    </Show>
  );
}
