import { vi } from "vitest";

type Listener = (ev: MessageEvent<string>) => void;

/** Scriptable stand-in for EventSource (jsdom has none). */
export class MockEventSource {
  static instances: MockEventSource[] = [];
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSED = 2;

  readonly url: string;
  readonly withCredentials: boolean;
  readyState = 0;
  closed = false;
  onopen: ((ev: Event) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;
  private listeners = new Map<string, Listener[]>();

  constructor(url: string, init?: EventSourceInit) {
    this.url = url;
    this.withCredentials = !!init?.withCredentials;
    MockEventSource.instances.push(this);
  }

  addEventListener(type: string, fn: Listener): void {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), fn]);
  }

  close(): void {
    this.readyState = 2;
    this.closed = true;
  }

  // ---- test controls ----

  open(): void {
    this.readyState = 1;
    this.onopen?.(new Event("open"));
  }

  emit(type: string, data: unknown, id?: string): void {
    const ev = new MessageEvent<string>(type, { data: typeof data === "string" ? data : JSON.stringify(data), lastEventId: id ?? "" });
    for (const fn of this.listeners.get(type) ?? []) fn(ev);
  }

  /** Connection dropped; the browser will retry by itself (readyState CONNECTING). */
  drop(): void {
    this.readyState = 0;
    this.onerror?.(new Event("error"));
  }

  /** Server answered with an error status; the browser gives up (readyState CLOSED). */
  fail(): void {
    this.readyState = 2;
    this.onerror?.(new Event("error"));
  }

  static latest(): MockEventSource {
    const es = MockEventSource.instances[MockEventSource.instances.length - 1];
    if (!es) throw new Error("no EventSource was created");
    return es;
  }
}

export function installEventSource(): typeof MockEventSource {
  MockEventSource.instances = [];
  vi.stubGlobal("EventSource", MockEventSource);
  return MockEventSource;
}
