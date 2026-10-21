import { createStore, reconcile } from "solid-js/store";
import {
  getGymClock,
  getGymState,
  sendGymCommand,
  type GymClock,
  type GymCommand,
  type GymResponse,
  type GymView,
} from "../api/gym";
import { createEngineChannel, currentTimeZone } from "./engine";

export interface GymClockState {
  rest_remaining: number;
  active_duration: string;
  active_elapsed_seconds?: number;
  /** When this clock snapshot arrived, for visual ticking only. */
  receivedAt: number;
}

interface GymStoreState {
  view: GymView | null;
  loaded: boolean;
  loadError: string | null;
  actionError: string | null;
  clock: GymClockState | null;
}

const [gym, setGym] = createStore<GymStoreState>({
  view: null,
  loaded: false,
  loadError: null,
  actionError: null,
  clock: null,
});

export { gym };

const channel = createEngineChannel<GymView, GymCommand, GymResponse>({
  getState: () => getGymState(currentTimeZone()),
  send: sendGymCommand,
  revisionOf: (view) => view.state.revision,
  onView: (view, receivedAt) => {
    setGym("view", reconcile(view));
    setGym("clock", {
      rest_remaining: view.rest_remaining,
      active_duration: view.summary.active_duration,
      active_elapsed_seconds: view.summary.active_elapsed_seconds,
      receivedAt,
    });
    setGym({ loaded: true, loadError: null });
  },
  onClock: (clock, receivedAt) => {
    const c = clock as GymClock;
    if (!c || typeof c !== "object") return;
    setGym("clock", {
      rest_remaining: c.rest_remaining,
      active_duration: c.active_duration,
      active_elapsed_seconds: c.active_elapsed_seconds,
      receivedAt,
    });
  },
  onLoadError: (message) => setGym("loadError", message),
  onActionError: (message) => setGym("actionError", message),
});

export function refreshGym(): Promise<void> {
  return channel.refresh().then(() => undefined);
}

/** The returned promise is the per-action pending signal for views. */
export function runGym(command: GymCommand): Promise<GymResponse | null> {
  setGym("actionError", null);
  return channel.run(command);
}

let clockSeq = 0;

/** Best-effort refresh of the rest timer and duration via GET /v1/gym/clock. */
export async function tickClock(): Promise<void> {
  const mine = ++clockSeq;
  try {
    const c = await getGymClock(currentTimeZone());
    if (mine !== clockSeq || !gym.view) return;
    setGym("clock", {
      rest_remaining: c.rest_remaining,
      active_duration: c.active_duration,
      active_elapsed_seconds: c.active_elapsed_seconds,
      receivedAt: Date.now(),
    });
  } catch {
    // Clock ticks stay quiet; the next view or tick corrects the display.
  }
}

export function dismissGymError(): void {
  setGym("actionError", null);
}

export function resetGym(): void {
  clockSeq++;
  channel.reset();
  setGym({ view: null, loaded: false, loadError: null, actionError: null, clock: null });
}

/** Visual countdown only; the server's clock remains the source of truth. */
export function restRemainingNow(
  clock: { rest_remaining: number; receivedAt: number } | null,
  nowMs: number,
): number {
  if (!clock) return 0;
  return Math.max(0, clock.rest_remaining - Math.floor((nowMs - clock.receivedAt) / 1000));
}
