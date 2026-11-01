import { api } from "../api/secretary";
import { toApiError } from "../api/client";
import { parsePage, type TimelineItem } from "../api/timeline";
import { getState, putItems, resetScope, setState, type Scope } from "./chatdb";
export interface Conversation {
  key: string; kind: "device" | "legacy" | "agent" | "telegram" | "discord" | "alexa" | "lucina" | "voice";
  title: string; channel: string; device?: string; deviceName?: string; caller?: string;
  count: number; lastId: string; lastTs: number | string; lastRole: string; lastText: string;
}
export function filterFor(scope: Scope) { return scope === "mine" ? { scope: "mine" } : { conversation: scope.slice(13) }; }
export async function syncScope(scope: Scope, device?: string | null, valid = () => true) {
  const cursor = await getState<string>(scope);
  const items: TimelineItem[] = [];
  if (cursor) {
    let after = cursor;
    for (let n = 0; n < 10; n++) {
      try {
        const raw = await api.getTimeline(null, 50, { ...filterFor(scope), after });
        if (!valid()) return { items: [], nextBefore: null, reset: false };
        const page = parsePage(raw);
        items.push(...page.items);
        await putItems(page.items);
        if (raw.cursor) after = raw.cursor;
        if (!raw.hasMore) {
          await setState(scope, after);
          return { items, nextBefore: page.nextBefore, reset: false };
        }
      } catch (e) { if (toApiError(e).status !== 410) throw e; break; }
    }
  }
  if (!valid()) return { items: [], nextBefore: null, reset: false };
  if (cursor) await setState(scope, null);
  const raw = await api.getTimeline(null, 50, filterFor(scope));
  const page = parsePage(raw);
  if (!valid()) return { items: [], nextBefore: null, reset: false };
  if (cursor) await resetScope(scope, device);
  await putItems(page.items);
  await setState(scope, raw.cursor ?? null);
  return { ...page, reset: !!cursor };
}
export async function olderScope(scope: Scope, before: string) {
  const page = parsePage(await api.getTimeline(before, 50, filterFor(scope)));
  await putItems(page.items);
  return page;
}
