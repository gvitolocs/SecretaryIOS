// The composer's draft: what "Close" saves, and the rule that an empty note is discarded.

import type { ChecklistItem, KeepColor, NoteInput, Reminder } from "../api/keep";

export interface Draft {
  title: string;
  content: string;
  /** Present = a list note. */
  items?: ChecklistItem[];
  color: KeepColor;
  pinned: boolean;
  archived: boolean;
  /** Display names. */
  labels: string[];
  reminder?: Reminder;
  collaborators: string[];
  files: File[];
}

export const emptyDraft = (): Draft => ({ title: "", content: "", color: "default", pinned: false, archived: false, labels: [], collaborators: [], files: [] });

/** Keep discards a note with no title, no text, no list item text and no image; colour, labels, pin or a reminder alone do not count. */
export function draftHasContent(d: Draft): boolean {
  if (d.title.trim()) return true;
  if (d.files.length) return true;
  if (d.items) return d.items.some((i) => i.text.trim() !== "");
  return d.content.trim() !== "";
}

const stripExt = (name: string): string => name.replace(/\.[^./]+$/, "");

/** The POST body for a draft; null when the draft is empty and must be discarded. */
export function draftToInput(d: Draft): NoteInput | null {
  if (!draftHasContent(d)) return null;
  const input: NoteInput = { color: d.color, pinned: d.pinned, archived: d.archived };
  let title = d.title.trim();
  if (d.items) {
    const items = d.items.filter((i) => i.text.trim() !== "").map((i) => ({ ...i, text: i.text.trim() }));
    input.items = items;
  } else input.content = d.content.replace(/\s+$/, "");
  // The server wants a title, text or items; a note that is only an image is titled after the file.
  if (!title && !input.content && !(input.items && input.items.length) && d.files[0]) title = stripExt(d.files[0].name).slice(0, 200);
  input.title = title;
  if (d.labels.length) input.labels = d.labels;
  if (d.collaborators.length) input.collaborators = d.collaborators;
  if (d.reminder) input.reminder = d.reminder;
  return input;
}
