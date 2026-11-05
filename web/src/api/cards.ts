// Which timeline items' buttons are closed, the same for every client (GET/POST /v1/timeline/cards).

import { request } from "./client";

export interface ServerCard {
  closed: boolean;
  status?: string;
  action?: string;
}

export function parseCards(data: unknown): Record<string, ServerCard> {
  const out: Record<string, ServerCard> = {};
  const cards = (data as { cards?: unknown } | null)?.cards;
  if (!cards || typeof cards !== "object") return out;
  for (const [id, raw] of Object.entries(cards as Record<string, unknown>)) {
    const c = raw as { closed?: unknown; status?: unknown; action?: unknown } | null;
    if (c?.closed !== true) continue;
    out[id] = {
      closed: true,
      ...(typeof c.status === "string" ? { status: c.status } : {}),
      ...(typeof c.action === "string" ? { action: c.action } : {}),
    };
  }
  return out;
}

export const cardsApi = {
  list: async () => parseCards(await request<unknown>("GET", "/v1/timeline/cards")),
  choose: (item: string, action: string) =>
    request<unknown>("POST", `/v1/timeline/cards/${encodeURIComponent(item)}`, { body: { action } }),
};
