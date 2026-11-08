// Test-only in-memory Shopping v2 server for stubFetch (never imported by the app).

import { fold } from "../../lib/shoppingCatalog";
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
    { id: "banana", name: "Banane", names: { it: "Banane", en: "Bananas", da: "Bananer" }, aliases: ["banana"], section: "produce", icon: "🍌" },
    { id: "milk", name: "Latte", names: { it: "Latte", en: "Milk", da: "Mælk" }, aliases: [], section: "dairy", icon: "🥛" },
    { id: "oat-milk", name: "Latte d'avena", names: { it: "Latte d'avena", en: "Oat milk" }, aliases: ["avena"], section: "dairy", icon: "🥛" },
    { id: "coffee", name: "Caffè", names: { it: "Caffè", en: "Coffee" }, aliases: ["espresso"], section: "pantry", icon: "☕" },
    { id: "pasta", name: "Pasta", names: { it: "Pasta", en: "Pasta" }, aliases: ["spaghetti"], section: "pantry", icon: "🍝" },
  ],
};

let seq = 0;
export function testItem(p: Partial<ShopItem> & { name: string }): ShopItem {
  seq += 1;
  return {
    id: `i${seq}`,
    objectId: p.catalogId ?? null,
    kind: p.catalogId ? "catalog" : null,
    names: {},
    catalogId: null,
    typedName: p.name,
    needsTranslation: false,
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
  // Like Lucina: every row gets an object; unknown text becomes a custom object named in every language.
  let customSeq = 0;
  const nameObjects = (l: FakeList) => {
    for (const row of l.items) {
      if (row.objectId) continue;
      const known = [...lists.values()].flatMap((x) => x.items).find((r) => r.kind === "custom" && r.objectId && fold(r.name) === fold(row.name));
      row.objectId = known?.objectId ?? `u_FAKE${String(++customSeq).padStart(22, "0")}`;
      row.kind = "custom";
      row.names = known?.names ?? { it: row.name, en: row.name, da: row.name };
      row.section = row.section ?? "other";
    }
  };
  const wire = (row: ShopItem, lang: string | null) => ({
    ...row,
    name: (lang ? row.names[lang] : undefined) ?? row.name,
    itemId: row.objectId,
    item: { id: row.objectId, kind: row.kind, names: row.names, section: row.section, icon: row.icon },
  });
  const state = (l: FakeList, lang: string | null) => ({
    list: { id: l.id, name: l.name, theme: l.theme },
    revision: l.revision,
    items: l.items.filter(isActive).map((r) => wire(r, lang)),
  });
  const catalogHint = (id: string) => {
    const c = TEST_CATALOG.items.find((i) => i.id === id);
    if (c) return { name: c.name, names: { it: c.name, ...c.names }, kind: "catalog" as const, section: c.section, icon: c.icon };
    const custom = [...lists.values()].flatMap((x) => x.items).find((r) => r.objectId === id && r.kind === "custom");
    return custom ? { name: custom.name, names: custom.names, kind: "custom" as const, section: custom.section, icon: custom.icon ?? "🛒" } : undefined;
  };
  const resolve = (text: string) => {
    const q = fold(text);
    const c = TEST_CATALOG.items.find((i) => [i.name, ...Object.values(i.names), ...i.aliases].some((n) => fold(n) === q));
    if (c) return c.id;
    return [...lists.values()].flatMap((x) => x.items).find((r) => r.kind === "custom" && Object.values(r.names).some((n) => n && fold(n) === q))?.objectId ?? undefined;
  };

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
      if (url.pathname === "/v1/shopping/items") {
        const seen = new Map<string, ShopItem>();
        for (const r of [...lists.values()].flatMap((x) => x.items)) if (r.kind === "custom" && r.objectId) seen.set(r.objectId, r);
        return {
          body: [...seen.values()].map((r) => ({ id: r.objectId, kind: "custom", names: r.names, aliases: [r.typedName], section: r.section, icon: r.icon })),
        };
      }
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
        if (call.method === "GET") return { body: state(list, url.searchParams.get("lang")) };
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
        const body = call.body as { expectedRevision: number; ops: Op[]; lang?: string };
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
          catalog: catalogHint,
          resolve,
          lang: body.lang ?? "it",
        });
        list.items = res.items;
        nameObjects(list);
        if (res.applied.length) list.revision += 1;
        return {
          body: {
            revision: list.revision,
            applied: res.applied,
            duplicates: res.duplicates,
            items: list.items.filter(isActive).map((r) => wire(r, body.lang ?? null)),
          },
        };
      }
      return { status: 404, body: { error: { code: "not_found", message: "Not found." } } };
    },
  };
  return server;
}
