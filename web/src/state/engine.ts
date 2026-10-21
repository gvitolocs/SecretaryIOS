import { toApiError, type ApiError } from "../api/client";
import { uuid } from "../lib/ids";
import { markLucinaOnline, markServerOffline } from "./lucina";

// Shared request/queue/conflict-retry logic (docs/server-api.md, command protocol) for the Gym and Body engines.
// Every state-changing command is serialised through one FIFO queue, so each
// command sees the revision produced by the previous one.

export interface EngineEnvelope<C> {
  op_id: string;
  expected_revision: number;
  timezone: string;
  command: C;
}

export interface EngineResponseShape<V> {
  ok: boolean;
  replayed?: boolean;
  code?: string;
  error?: string;
  view?: V;
  clock?: unknown;
}

export interface EngineChannelOptions<V, C, R extends EngineResponseShape<V>> {
  getState: () => Promise<V>;
  send: (envelope: EngineEnvelope<C>) => Promise<R>;
  revisionOf: (view: V) => number;
  onView: (view: V, receivedAt: number) => void;
  onClock?: (clock: unknown, receivedAt: number) => void;
  onLoadError: (message: string) => void;
  onActionError: (message: string) => void;
  onOnline?: () => void;
  onOffline?: () => void;
  conflictMessage?: string;
}

export interface EngineChannel<V, C, R> {
  refresh(): Promise<V | null>;
  run(command: C): Promise<R | null>;
  query(command: C): Promise<R | null>;
  reset(): void;
}

const DEFAULT_CONFLICT = "This changed somewhere else. Refreshed \u2014 please try again.";
const OFFLINE_CODES = new Set(["lucina_unreachable", "lucina_timeout"]);

export function currentTimeZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
}

function isOffline(err: ApiError): boolean {
  return err.status === 502 || err.status === 504 || err.status === 0 || OFFLINE_CODES.has(err.code);
}

export function createEngineChannel<V, C, R extends EngineResponseShape<V>>(
  opts: EngineChannelOptions<V, C, R>,
): EngineChannel<V, C, R> {
  let revision: number | null = null;
  let generation = 0;
  let tail: Promise<unknown> = Promise.resolve();

  const online = () => (opts.onOnline ?? markLucinaOnline)();
  const offline = () => (opts.onOffline ?? markServerOffline)();

  function enqueue<T>(fn: () => Promise<T>): Promise<T> {
    const result = tail.then(fn);
    tail = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  }

  async function fetchState(): Promise<V | null> {
    const mine = generation;
    try {
      const view = await opts.getState();
      if (mine !== generation) return null;
      revision = opts.revisionOf(view);
      opts.onView(view, Date.now());
      online();
      return view;
    } catch (e) {
      if (mine !== generation) return null;
      const err = toApiError(e);
      if (isOffline(err)) offline();
      opts.onLoadError(err.message);
      return null;
    }
  }

  async function execute(command: C): Promise<R | null> {
    const mine = generation;
    if (revision === null) {
      const loaded = await fetchState();
      if (loaded === null || mine !== generation) return null;
    }
    // docs/server-api.md: on conflict, refetch and rebuild the command against
    // the fresh state with a NEW op_id (an op_id is never reused across states).
    let opId = uuid();
    let expected = revision as number;

    for (let attempt = 0; attempt < 2; attempt++) {
      let res: R;
      try {
        res = await opts.send({
          op_id: opId,
          expected_revision: expected,
          timezone: currentTimeZone(),
          command,
        });
      } catch (e) {
        if (mine !== generation) return null;
        const err = toApiError(e);
        if (err.status === 409 || err.code === "conflict") {
          if (attempt === 0) {
            const fresh = await fetchState();
            if (fresh === null || mine !== generation) return null;
            expected = revision as number;
            opId = uuid();
            continue;
          }
          opts.onActionError(opts.conflictMessage ?? DEFAULT_CONFLICT);
          return null;
        }
        if (isOffline(err)) {
          offline();
          return null;
        }
        opts.onActionError(err.message);
        return null;
      }

      if (mine !== generation) return null;
      if (res.view !== undefined) {
        revision = opts.revisionOf(res.view);
        opts.onView(res.view, Date.now());
      }
      if (res.clock !== undefined && opts.onClock) opts.onClock(res.clock, Date.now());

      if (res.ok) {
        online();
        return res;
      }
      if (res.code === "conflict") {
        if (attempt === 0) {
          const fresh = await fetchState();
          if (fresh === null || mine !== generation) return null;
          expected = revision as number;
          opId = uuid();
          continue;
        }
        opts.onActionError(opts.conflictMessage ?? DEFAULT_CONFLICT);
        return null;
      }
      opts.onActionError(res.error ?? "The change could not be saved.");
      return res;
    }
    return null;
  }

  async function query(command: C): Promise<R | null> {
    const mine = generation;
    try {
      const res = await opts.send({
        op_id: uuid(),
        expected_revision: revision ?? 0,
        timezone: currentTimeZone(),
        command,
      });
      if (mine !== generation) return null;
      if (res.clock !== undefined && opts.onClock) opts.onClock(res.clock, Date.now());
      if (res.view !== undefined) {
        revision = opts.revisionOf(res.view);
        opts.onView(res.view, Date.now());
      }
      return res;
    } catch {
      // Clock ticks are best effort and stay quiet.
      return null;
    }
  }

  function reset(): void {
    generation++;
    revision = null;
    tail = Promise.resolve();
  }

  return {
    refresh: () => enqueue(fetchState),
    run: (command) => enqueue(() => execute(command)),
    query,
    reset,
  };
}
