// Undo and redo for the editor, as immutable values: `past` (oldest first), the `present` and `future` (next redo
// first). A change that equals the present adds nothing, so a slider dragged back to where it was leaves no step.

export interface History<T> {
  past: T[];
  present: T;
  future: T[];
}

/** The most undo steps kept. */
export const MAX_HISTORY = 100;

export function startHistory<T>(present: T): History<T> {
  return { past: [], present, future: [] };
}

export function commit<T>(h: History<T>, next: T, same: (a: T, b: T) => boolean): History<T> {
  if (same(h.present, next)) return h;
  return { past: [...h.past, h.present].slice(-MAX_HISTORY), present: next, future: [] };
}

export const canUndo = <T,>(h: History<T>): boolean => h.past.length > 0;
export const canRedo = <T,>(h: History<T>): boolean => h.future.length > 0;

export function undo<T>(h: History<T>): History<T> {
  const previous = h.past[h.past.length - 1];
  if (previous === undefined) return h;
  return { past: h.past.slice(0, -1), present: previous, future: [h.present, ...h.future] };
}

export function redo<T>(h: History<T>): History<T> {
  const [next, ...rest] = h.future;
  if (next === undefined) return h;
  return { past: [...h.past, h.present], present: next, future: rest };
}
