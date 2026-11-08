import type { KeepNote } from "../api/keep";

/** A full note as the server returns it, with the fields a test cares about overridden. */
export function note(over: Partial<KeepNote> & { id: string }): KeepNote {
  return {
    slug: over.id,
    title: "",
    content: "",
    tags: [],
    color: "default",
    pinned: false,
    archived: false,
    attachments: [],
    annotations: [],
    collaborators: [],
    position: 1000,
    rev: 1,
    source: "web",
    createdAt: "2026-10-20T10:00:00.000Z",
    updatedAt: "2026-10-20T10:00:00.000Z",
    ...over,
  };
}

/** The server's JSON for a note (what the API client parses). */
export const wire = (n: KeepNote): Record<string, unknown> => ({ ...n });
