// The editor's state: the edit list being worked on, a live draft while a control is being dragged, and the undo
// history. Controls call `preview` while they move (the picture follows, nothing is recorded) and `commit` when
// they let go (one undo step per drag). Pure signals, no DOM, so the rules are tested without a browser.

import { createMemo, createSignal } from "solid-js";
import { canRedo, canUndo, commit as commitStep, redo as redoStep, startHistory, undo as undoStep } from "../../../lib/editHistory";
import { cloneEdit, isIdentity, sameEdit, tidy, type EditList } from "../../../lib/editList";

export function createEditorModel(initial: EditList) {
  const start = cloneEdit(initial);
  const [history, setHistory] = createSignal(startHistory(start));
  const [draft, setDraft] = createSignal<EditList | null>(null);

  /** What the picture shows: the draft while a control is moving, else the last committed list. */
  const list = createMemo(() => draft() ?? history().present);
  const committed = createMemo(() => history().present);

  return {
    list,
    committed,
    /** A control is being dragged (a draft is showing). */
    dragging: createMemo(() => draft() !== null),
    canUndo: createMemo(() => draft() !== null || canUndo(history())),
    canRedo: createMemo(() => draft() === null && canRedo(history())),
    /** Differs from what the editor opened with. */
    dirty: createMemo(() => !sameEdit(tidy(list()), tidy(start))),
    /** Would save as a revert. */
    identity: createMemo(() => isIdentity(tidy(list()))),

    /** Show `next` while a control moves; nothing is recorded. */
    preview(next: EditList) {
      setDraft(next);
    },
    /** Make `next` the list (one undo step). */
    commit(next: EditList) {
      setDraft(null);
      setHistory((h) => commitStep(h, tidy(next), (a, b) => sameEdit(a, b)));
    },
    /** Drop the draft: the control was let go without a change. */
    cancelPreview() {
      setDraft(null);
    },
    undo() {
      // A draft in the air is undone first.
      if (draft() !== null) setDraft(null);
      else setHistory((h) => undoStep(h));
    },
    redo() {
      if (draft() === null) setHistory((h) => redoStep(h));
    },
    /** The list to save. */
    result: (): EditList => tidy(list()),
  };
}

export type EditorModel = ReturnType<typeof createEditorModel>;
