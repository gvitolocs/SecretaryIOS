import type { LabJob } from "../api/gallery";

/** A synthetic Lab job (no real names or files). */
export function labJob(id: string, over: Partial<LabJob> = {}): LabJob {
  return {
    id,
    itemId: "itm_a",
    referenceItemId: null,
    prompt: "make the sky a sunset",
    status: "queued",
    phase: null,
    progress: 0,
    error: null,
    seed: null,
    steps: null,
    model: null,
    queuePosition: 1,
    createdAt: "2026-10-28T09:00:00.000Z",
    startedAt: null,
    finishedAt: null,
    acceptedItemId: null,
    width: 1024,
    height: 768,
    original: { id: "itm_a", title: "a.jpg", width: 4000, height: 3000, rotation: 0, thumbhash: null, trashed: false },
    urls: { before: "/v1/media/items/itm_a/thumb?size=1024&v=1", after: null, reference: null },
    ...over,
  };
}

/** The server's JSON for a job (snake_case), for the parser and the mocked endpoints. */
export function rawJob(id: string, over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id,
    item_id: "itm_a",
    reference_item_id: null,
    prompt: "make the sky a sunset",
    status: "queued",
    phase: null,
    progress: 0,
    error: null,
    seed: null,
    steps: null,
    model: null,
    queue_position: 1,
    created_at: "2026-10-28T09:00:00.000Z",
    started_at: null,
    finished_at: null,
    accepted_item_id: null,
    result: null,
    original: { id: "itm_a", title: "a.jpg", width: 4000, height: 3000, rotation: 0, thumbhash: null, trashed: false },
    urls: { before: "/v1/media/items/itm_a/thumb?size=1024&v=1", after: null, reference: null, result: null },
    ...over,
  };
}
