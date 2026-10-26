// Test-only in-memory Shopping v2 server for stubFetch (never imported by the app).

import { applyOps, isActive, type ListTheme, type Op, type ShopItem } from "../../lib/shoppingOps";
import type { Call, Handler } from "../../test/fetch";

export const TEST_CATALOG = {
  version: 1,
  lang: "it",
  sections: [
    { id: "produce", name: "Frutta e verdura", icon: "🥦", order: 1 },
    { id: "dairy", name: "Latticini", icon: "🧀", order: 2 },
    { id: "pantry", name: "Dispensa", icon: "🥫", order: 3 },
  ],
  items: [
    { id: "banana", name: "Banane", names: { en: "Bananas", da: "Bananer" }, aliases: ["banana"], section: "produce", icon: "🍌" },
    { id: "milk", name: "Latte", names: { en: "Milk", da: "Mælk" }, aliases: [], section: "dairy", icon: "🥛" },
    { id: "oat-milk", name: "Latte d'avena", names: { en: "Oat milk" }, aliases: ["avena"], section: "dairy", icon: "🥛" },
    { id: "coffee", name: "Caffè", names: { en: "Coffee" }, aliases: ["espresso"], section: "pantry", icon: "☕" },
    { id: "pasta", name: "Pasta", names: { en: "Pasta" }, aliases: ["spaghetti"], section: "pantry", icon: "🍝" },
  ],
};

let seq = 0;
export function testItem(p: Partial<ShopItem> & { name: string }): ShopItem {
  seq += 1;
  return {
    id: `i${seq}`,
    catalogId: null,
    spec: "",
    quantity: null,
    unit: null,
    section: null,
    icon: null,
    status: "needed",
    position: seq,
    updatedAt: "2026-10-21T10:00:00.000Z",
    updatedBy: "telegram",
    ...p,
  };
}

interface FakeList {
  id: string;
  name: string;
  theme: ListTheme;
  revision: number;
  items: ShopItem[];
  seen: Set<string>;
}

export interface FakeServer {
  lists: Map<string, FakeList>;
  calls: Call[];
  /** Reject every request as a network failure. */
  offline: boolean;
  /** Answer the next ops POST with 409 (after `beforeConflict` ran). */
  conflictNext: boolean;
  beforeConflict: (() => void) | null;
  handler: Handler;
  opsPosts: () => Array<{ listId: string; expectedRevision: number; ops: Op[] }>;
}

export function fakeShoppingServer(init: Record<string, { name: string; theme: ListTheme; items: ShopItem[] }>): FakeServer {
  const lists = new Map<string, FakeList>();
  for (const [id, l] of Object.entries(init)) lists.set(id, { id, ...l, revision: 1, seen: new Set() });
  let newIds = 0;

  const summary = (l: FakeList) => ({
    id: l.id,
    name: l.name,
    theme: l.theme,
    needed: l.items.filter((i) => i.status === "needed").length,
    recent: l.items.filter((i) => i.status === "recent").length,
    updatedAt: "2026-10-21T10:00:00.000Z",
  });
  const state = (l: FakeList) => ({
    list: { id: l.id, name: l.name, theme: l.theme },
    revision: l.revision,
    items: l.items.filter(isActive),
  });

  const server: FakeServer = {
    lists,
    calls: [],
    offline: false,
    conflictNext: false,
    beforeConflict: null,
    opsPosts: () =>
      server.calls
        .filter((c) => c.method === "POST" && /\/ops$/.test(c.url))
        .map((c) => ({ listId: c.url.split("/")[4] ?? "", ...(c.body as { expectedRevision: number; ops: Op[] }) })),
    handler: (call) => {
      server.calls.push(call);
      if (server.offline) throw new TypeError("Failed to fetch");
      const url = new URL(call.url, "http://test.local");
      const parts = url.pathname.split("/").filter(Boolean); // v1 shopping lists {id} ops
      if (url.pathname === "/v1/shopping/catalog") return { body: { ...TEST_CATALOG, lang: url.searchParams.get("lang") ?? "it" } };
      if (url.pathname === "/v1/shopping/lists") {
        if (call.method === "GET") return { body: [...lists.values()].map(summary) };
        const b = call.body as { name: string; theme: ListTheme };
        const l: FakeList = { id: `list-${++newIds}`, name: b.name, theme: b.theme, revision: 1, items: [], seen: new Set() };
        lists.set(l.id, l);
        return { status: 201, body: summary(l) };
      }
      const list = lists.get(decodeURIComponent(parts[3] ?? ""));
      if (!list) return { status: 404, body: { error: { code: "list_not_found", message: "No such list." } } };
      if (parts.length === 4) {
        if (call.method === "GET") return { body: state(list) };
        if (call.method === "PATCH") {
          Object.assign(list, call.body as object);
          return { body: summary(list) };
        }
        if (call.method === "DELETE") {
          lists.delete(list.id);
          return { body: { deleted: true } };
        }
      }
      if (parts[4] === "ops" && call.method === "POST") {
        const body = call.body as { expectedRevision: number; ops: Op[] };
        if (server.conflictNext) {
          server.conflictNext = false;
          server.beforeConflict?.();
          list.revision += 1;
          return { status: 409, body: { error: { code: "revision_conflict", message: "The list changed." } } };
        }
        const res = applyOps(list.items, body.ops, {
          listId: list.id,
          now: "2026-10-21T11:00:00.000Z",
          by: (call.headers["x-secretary-client"] as string) ?? "web",
          newId: () => `srv-${++newIds}`,
          seen: list.seen,
          catalog: (id) => {
            const c = TEST_CATALOG.items.find((i) => i.id === id);
            return c ? { name: c.name, section: c.section, icon: c.icon } : undefined;
          },
        });
        list.items = res.items;
        if (res.applied.length) list.revision += 1;
        return { body: { revision: list.revision, applied: res.applied, duplicates: res.duplicates, items: list.items.filter(isActive) } };
      }
      return { status: 404, body: { error: { code: "not_found", message: "Not found." } } };
    },
  };
  return server;
}
