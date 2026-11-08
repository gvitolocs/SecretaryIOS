// Keep's note actions with Keep's snackbar messages ("Note archived · Undo").

import type { KeepColor, KeepNote } from "../api/keep";
import { t } from "../i18n";
import { keep, showSnackbar } from "./keep";

const some = (ids: string[]): KeepNote[] => ids.map((id) => keep.note(id)).filter((n): n is KeepNote => !!n);

export function archive(ids: string[]): void {
  const before = some(ids);
  if (!before.length) return;
  const pinned = before.filter((n) => n.pinned).map((n) => n.id);
  // Keep unpins what it archives.
  void keep.updateMany(ids, { archived: true, pinned: false });
  showSnackbar(t("keep.noteArchived", { count: before.length }), {
    label: t("keep.undo"),
    run: () => {
      void keep.updateMany(ids, { archived: false });
      if (pinned.length) void keep.updateMany(pinned, { pinned: true });
    },
  });
}

export function unarchive(ids: string[]): void {
  if (!ids.length) return;
  void keep.updateMany(ids, { archived: false });
  showSnackbar(t("keep.noteUnarchived", { count: ids.length }), { label: t("keep.undo"), run: () => void keep.updateMany(ids, { archived: true }) });
}

export function trash(ids: string[]): void {
  const n = some(ids).length;
  if (!n) return;
  void keep.trash(ids);
  showSnackbar(t("keep.noteTrashed", { count: n }), { label: t("keep.undo"), run: () => void keep.restore(ids) });
}

export function restore(ids: string[]): void {
  if (!ids.length) return;
  void keep.restore(ids);
  showSnackbar(t("keep.noteRestored", { count: ids.length }), { label: t("keep.undo"), run: () => void keep.trash(ids) });
}

export function deleteForever(ids: string[]): void {
  if (!ids.length) return;
  void keep.deleteForever(ids);
  showSnackbar(t("keep.noteDeleted", { count: ids.length }));
}

export function setColor(ids: string[], color: KeepColor): void {
  void keep.updateMany(ids, { color });
}

export function setPinned(ids: string[], pinned: boolean): void {
  if (!ids.length) return;
  const before = some(ids).filter((n) => n.pinned !== pinned).map((n) => n.id);
  void keep.updateMany(ids, pinned ? { pinned: true, archived: false } : { pinned: false });
  showSnackbar(t(pinned ? "keep.notePinned" : "keep.noteUnpinned"), before.length ? { label: t("keep.undo"), run: () => void keep.updateMany(before, { pinned: !pinned }) } : undefined);
}

export async function copyNote(id: string): Promise<void> {
  const made = await keep.duplicate(id);
  if (made) showSnackbar(t("keep.noteCopied"));
}
