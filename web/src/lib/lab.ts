// Pure helpers for the AI Lab (views/GalleryLab.tsx, components/gallery/lab/*): prompt rules, the suggestions, how often to
// ask the server, and what a job card says about itself. No DOM here.

import { LAB_PROMPT_MAX, type LabJob, type LabStatus } from "../api/gallery";
import type { MessageKey } from "../i18n";

export const LAB_SUGGESTIONS = [
  "lab.suggestion1",
  "lab.suggestion2",
  "lab.suggestion3",
  "lab.suggestion4",
  "lab.suggestion5",
  "lab.suggestion6",
] as const satisfies readonly MessageKey[];

/** A prompt the server will take: 1 to 1000 characters once trimmed. */
export function promptValid(prompt: string): boolean {
  const n = [...prompt.trim()].length;
  return n >= 1 && n <= LAB_PROMPT_MAX;
}

/** Add a suggestion to what is typed: it replaces an empty field and follows other text after a full stop. */
export function withSuggestion(current: string, suggestion: string): string {
  const text = current.trim();
  if (!text) return suggestion;
  const sep = /[.!?]$/.test(text) ? " " : ". ";
  return `${text}${sep}${suggestion}`;
}

/** Jobs that are still moving: the Lab asks the server often while any is. */
export function isActive(job: Pick<LabJob, "status">): boolean {
  return job.status === "queued" || job.status === "running";
}

export const POLL_ACTIVE_MS = 2500;
export const POLL_IDLE_MS = 20_000;

export function pollDelay(jobs: readonly Pick<LabJob, "status">[]): number {
  return jobs.some(isActive) ? POLL_ACTIVE_MS : POLL_IDLE_MS;
}

/** The badge text key for a job. */
export function statusKey(status: LabStatus): MessageKey {
  switch (status) {
    case "queued":
      return "lab.statusQueued";
    case "running":
      return "lab.statusRunning";
    case "done":
      return "lab.statusDone";
    case "failed":
      return "lab.statusFailed";
    default:
      return "lab.statusAccepted";
  }
}

export type LabLine =
  | { key: "lab.queuePosition"; params: { position: number } }
  | { key: "lab.waitingGpu" }
  | { key: "lab.starting" }
  | { key: "lab.generating"; params: { percent: number } };

/** What a queued or running job says under its picture. */
export function progressLine(job: Pick<LabJob, "status" | "phase" | "progress" | "queuePosition">): LabLine | null {
  if (job.status === "queued") return job.queuePosition && job.queuePosition > 0 ? { key: "lab.queuePosition", params: { position: job.queuePosition } } : { key: "lab.starting" };
  if (job.status !== "running") return null;
  if (job.phase === "waiting") return { key: "lab.waitingGpu" };
  if (job.progress <= 0) return { key: "lab.starting" };
  return { key: "lab.generating", params: { percent: job.progress } };
}

/** Jobs in the order the Lab shows them: working first, then the queue, then the finished ones, newest first. */
export function sortJobs(jobs: readonly LabJob[]): LabJob[] {
  const rank = (j: LabJob) => (j.status === "running" ? 0 : j.status === "queued" ? 1 : j.status === "done" ? 2 : j.status === "failed" ? 3 : 4);
  return [...jobs].sort((a, b) => {
    const r = rank(a) - rank(b);
    if (r !== 0) return r;
    // The queue runs oldest first; everything else shows newest first.
    return a.status === "queued" ? a.createdAt.localeCompare(b.createdAt) : b.createdAt.localeCompare(a.createdAt);
  });
}

/** The jobs waiting for the owner's decision (shown as a count on the Lab link). */
export function readyCount(jobs: readonly Pick<LabJob, "status">[]): number {
  return jobs.filter((j) => j.status === "done").length;
}

/** A divider position (0 to 100) from a pointer x inside a box, clamped. */
export function splitFromPointer(clientX: number, left: number, width: number): number {
  if (width <= 0) return 50;
  return Math.min(100, Math.max(0, ((clientX - left) / width) * 100));
}
