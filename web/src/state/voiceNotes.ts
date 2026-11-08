// Voice notes on their way to the timeline: the bubble shown under the chat until the server's own
// timeline item (the one `note` names, `userItemId`) arrives. `transcript` fills it in early.

import { createSignal } from "solid-js";

export interface PendingNote {
  key: string;
  transcript?: string;
  /** Set when the server's `note` message came. */
  userItemId?: string;
}

/** After `note` the timeline stream should bring the item within this; if not, the bubble goes. */
export const PENDING_NOTE_TTL_MS = 20_000;

const [pendingNotes, setPendingNotes] = createSignal<PendingNote[]>([]);
export { pendingNotes };

let n = 0;
const timers = new Map<string, ReturnType<typeof setTimeout>>();

export function addPendingNote(): string {
  const key = `vn-${Date.now()}-${++n}`;
  setPendingNotes((l) => [...l, { key }]);
  return key;
}

export function updatePendingNote(key: string, patch: Partial<Omit<PendingNote, "key">>): void {
  setPendingNotes((l) => l.map((p) => (p.key === key ? { ...p, ...patch } : p)));
  if (patch.userItemId) {
    clearTimeout(timers.get(key));
    timers.set(key, setTimeout(() => removePendingNote(key), PENDING_NOTE_TTL_MS));
  }
}

export function removePendingNote(key: string): void {
  clearTimeout(timers.get(key));
  timers.delete(key);
  setPendingNotes((l) => l.filter((p) => p.key !== key));
}

/** The pending notes whose timeline item is not in the chat yet. */
export function stillPending(list: readonly PendingNote[], hasItem: (id: string) => boolean): PendingNote[] {
  return list.filter((p) => !p.userItemId || !hasItem(p.userItemId));
}
