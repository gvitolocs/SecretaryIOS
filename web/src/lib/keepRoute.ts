// Keep's URLs: /keep (Notes), /keep/reminders, /keep/archive, /keep/trash, /keep/label/<name>,
// and /keep/note/<id> (the note dialog over Notes). From another view the dialog is ?note=<id>.

import { normLabel } from "../api/keep";
import type { KeepView } from "./keepFilter";

export interface KeepRoute {
  view: KeepView;
  noteId?: string;
}

const dec = (s: string): string => {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
};

export function parseKeepPath(pathname: string, search = ""): KeepRoute {
  const parts = pathname.replace(/^\/keep\/?/, "").split("/").filter(Boolean).map(dec);
  const q = new URLSearchParams(search).get("note") ?? undefined;
  const route = (view: KeepView, noteId?: string): KeepRoute => (noteId ? { view, noteId } : { view });
  switch (parts[0]) {
    case "reminders":
      return route({ kind: "reminders" }, q);
    case "archive":
      return route({ kind: "archive" }, q);
    case "trash":
      return route({ kind: "trash" }, q);
    case "label":
      return route({ kind: "label", tag: normLabel(parts[1] ?? "") }, q);
    case "note":
      return route({ kind: "notes" }, parts[1] || q);
    default:
      return route({ kind: "notes" }, q);
  }
}

/** The path of a view; a label needs its display name for a readable URL. */
export function keepHref(view: KeepView, labelName?: string): string {
  switch (view.kind) {
    case "notes":
      return "/keep";
    case "reminders":
      return "/keep/reminders";
    case "archive":
      return "/keep/archive";
    case "trash":
      return "/keep/trash";
    case "label":
      return `/keep/label/${encodeURIComponent(labelName ?? view.tag)}`;
  }
}

export const sameView = (a: KeepView, b: KeepView): boolean => a.kind === b.kind && (a.kind !== "label" || (b.kind === "label" && a.tag === b.tag));

/** The URL that opens a note's dialog over the current view. */
export function noteHref(id: string, view: KeepView, labelName?: string): string {
  if (view.kind === "notes") return `/keep/note/${encodeURIComponent(id)}`;
  return `${keepHref(view, labelName)}?note=${encodeURIComponent(id)}`;
}
