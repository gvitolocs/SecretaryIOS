// Shopping v2 mock (docs/shopping-v2.md), registered from mock/server.mjs with a
// single line after the session check. In-memory lists, items, revisions and
// op-id dedup; the catalog comes from assets/catalog/catalog.<lang>.json (or
// catalog.v1.json) when it exists, else from the small inline sample below.
//
//   GET    /v1/shopping/catalog?lang=it|en|da
//   GET    /v1/shopping/lists                 POST /v1/shopping/lists {name, theme}
//   GET    /v1/shopping/lists/{id}            PATCH/DELETE /v1/shopping/lists/{id}
//   POST   /v1/shopping/lists/{id}/ops {expectedRevision, lang?, ops}   (list GET and ops take ?lang=it|en|da)
//   GET    /v1/shopping/items                 the custom item objects
// Every row points to an item object (a catalog item or a custom object with
// names in it/en/da); unknown text becomes a custom object (the mock names
// every language with the typed text and marks it needsTranslation).
// Test hooks (session + CSRF):
//   POST /v1/shopping/_mock/bump {listId?}     touches the first needed item, so a stale op on it gets 409
//   POST /v1/shopping/_mock/external {listId?, name, spec?, via?}  adds an item as if from Telegram
//   POST /v1/shopping/_mock/down {down: true}  v2 endpoints answer 503 until {down: false}
//
// `add` takes `itemId` (an object id) or `text`; a client row id may come as `rowId`
// (or, from old outboxes, as a UUID in `itemId`).

import { readFileSync } from "node:fs";
import { randomUUID } from "node:crypto";

const THEMES = ["home", "grocery", "work", "holiday", "garden", "hardware", "baby", "other"];
const SOURCES = ["telegram", "discord", "web", "ios", "alexa", "lucina"];
const DEFAULT_LIST = "default";
const MAX_OPS = 200;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const iso = () => new Date().toISOString();

// ---------- Catalog ----------

const SAMPLE_SECTIONS = [
  ["produce", "🥦", { it: "Frutta e verdura", en: "Fruit & vegetables", da: "Frugt og grønt" }],
  ["bakery", "🥖", { it: "Pane e forno", en: "Bakery", da: "Brød og bagværk" }],
  ["dairy", "🧀", { it: "Latticini e uova", en: "Dairy & eggs", da: "Mejeri og æg" }],
  ["meat", "🥩", { it: "Carne e pesce", en: "Meat & fish", da: "Kød og fisk" }],
  ["pantry", "🥫", { it: "Dispensa", en: "Pantry", da: "Kolonial" }],
  ["drinks", "🧃", { it: "Bevande", en: "Drinks", da: "Drikkevarer" }],
  ["frozen", "🧊", { it: "Surgelati", en: "Frozen", da: "Frost" }],
  ["household", "🧽", { it: "Casa e pulizia", en: "Household", da: "Husholdning" }],
  ["personal", "🧴", { it: "Cura della persona", en: "Personal care", da: "Personlig pleje" }],
  ["hardware", "🔧", { it: "Ferramenta", en: "Hardware", da: "Byggemarked" }],
];

// [id, section, icon, it, en, da, aliases]
const SAMPLE_ITEMS = [
  ["banana", "produce", "🍌", "Banane", "Bananas", "Bananer", ["banana"]],
  ["apple", "produce", "🍎", "Mele", "Apples", "Æbler", ["mela"]],
  ["tomato", "produce", "🍅", "Pomodori", "Tomatoes", "Tomater", ["pomodoro"]],
  ["lemon", "produce", "🍋", "Limoni", "Lemons", "Citroner", ["limone"]],
  ["potato", "produce", "🥔", "Patate", "Potatoes", "Kartofler", ["patata"]],
  ["onion", "produce", "🧅", "Cipolle", "Onions", "Løg", ["cipolla"]],
  ["basil", "produce", "🌿", "Basilico", "Basil", "Basilikum", []],
  ["salad", "produce", "🥬", "Insalata", "Lettuce", "Salat", ["lattuga"]],
  ["bread", "bakery", "🍞", "Pane", "Bread", "Brød", ["pagnotta"]],
  ["croissant", "bakery", "🥐", "Cornetti", "Croissants", "Croissanter", ["brioche", "cornetto"]],
  ["milk", "dairy", "🥛", "Latte", "Milk", "Mælk", ["latte intero"]],
  ["oat-milk", "dairy", "🥛", "Latte d'avena", "Oat milk", "Havremælk", ["bevanda all'avena", "avena"]],
  ["eggs", "dairy", "🥚", "Uova", "Eggs", "Æg", ["uovo"]],
  ["butter", "dairy", "🧈", "Burro", "Butter", "Smør", []],
  ["yogurt", "dairy", "🥣", "Yogurt", "Yoghurt", "Yoghurt", ["yoghurt"]],
  ["parmesan", "dairy", "🧀", "Parmigiano", "Parmesan", "Parmesan", ["grana", "parmigiano reggiano"]],
  ["mozzarella", "dairy", "🧀", "Mozzarella", "Mozzarella", "Mozzarella", ["fior di latte"]],
  ["chicken", "meat", "🍗", "Pollo", "Chicken", "Kylling", ["petto di pollo"]],
  ["salmon", "meat", "🐟", "Salmone", "Salmon", "Laks", []],
  ["pasta", "pantry", "🍝", "Pasta", "Pasta", "Pasta", ["spaghetti", "penne"]],
  ["rice", "pantry", "🍚", "Riso", "Rice", "Ris", ["carnaroli", "arborio"]],
  ["coffee", "pantry", "☕", "Caffè", "Coffee", "Kaffe", ["caffe in grani", "espresso"]],
  ["olive-oil", "pantry", "🫒", "Olio d'oliva", "Olive oil", "Olivenolie", ["olio", "evo"]],
  ["sugar", "pantry", "🍬", "Zucchero", "Sugar", "Sukker", []],
  ["flour", "pantry", "🌾", "Farina", "Flour", "Mel", ["farina 00"]],
  ["passata", "pantry", "🥫", "Passata di pomodoro", "Tomato passata", "Tomatpassata", ["sugo"]],
  ["water", "drinks", "💧", "Acqua", "Water", "Vand", ["acqua frizzante", "acqua naturale"]],
  ["beer", "drinks", "🍺", "Birra", "Beer", "Øl", []],
  ["wine", "drinks", "🍷", "Vino", "Wine", "Vin", ["vino rosso"]],
  ["ice-cream", "frozen", "🍨", "Gelato", "Ice cream", "Is", []],
  ["peas", "frozen", "🫛", "Piselli surgelati", "Frozen peas", "Frosne ærter", ["piselli"]],
  ["sponge", "household", "🧽", "Spugne", "Sponges", "Svampe", ["spugna"]],
  ["dish-soap", "household", "🫧", "Detersivo piatti", "Dish soap", "Opvaskemiddel", ["sapone piatti"]],
  ["toilet-paper", "household", "🧻", "Carta igienica", "Toilet paper", "Toiletpapir", ["carta"]],
  ["toothpaste", "personal", "🪥", "Dentifricio", "Toothpaste", "Tandpasta", []],
  ["shampoo", "personal", "🧴", "Shampoo", "Shampoo", "Shampoo", []],
  ["screws", "hardware", "🔩", "Viti", "Screws", "Skruer", ["vite"]],
  ["light-bulb", "hardware", "💡", "Lampadine", "Light bulbs", "Pærer", ["lampadina"]],
  ["batteries", "hardware", "🔋", "Batterie", "Batteries", "Batterier", ["pile", "stilo"]],
];

function sampleCatalog(lang) {
  return {
    version: 1,
    lang,
    sections: SAMPLE_SECTIONS.map(([id, icon, names], i) => ({ id, name: names[lang] ?? names.it, icon, order: i + 1 })),
    items: SAMPLE_ITEMS.map(([id, section, icon, it, en, da, aliases]) => {
      const names = { it, en, da };
      return { id, name: names[lang] ?? it, names, aliases, section, icon };
    }),
  };
}

function readCatalogFile(name) {
  try {
    return JSON.parse(readFileSync(new URL(`../../assets/catalog/${name}`, import.meta.url), "utf8"));
  } catch {
    return null;
  }
}

/** Re-read on every request so a catalog written later is picked up without a restart. */
function catalogFor(lang) {
  const own = readCatalogFile(`catalog.${lang}.json`);
  if (own && Array.isArray(own.items)) return { ...own, lang };
  const v1 = readCatalogFile("catalog.v1.json");
  if (v1 && Array.isArray(v1.items)) {
    return {
      ...v1,
      lang,
      items: v1.items.map((i) => ({ ...i, name: i?.names?.[lang] ?? i.name })),
    };
  }
  // Fall back to the Italian file, localising names where it has them.
  const it = lang !== "it" ? readCatalogFile("catalog.it.json") : null;
  if (it && Array.isArray(it.items)) {
    return { ...it, lang, items: it.items.map((i) => ({ ...i, name: i?.names?.[lang] ?? i.name })) };
  }
  return sampleCatalog(lang);
}

function catalogItem(id) {
  if (!id) return null;
  return catalogFor("it").items.find((i) => i && i.id === id) ?? null;
}

// ---------- Text folding (same rules as src/lib/shoppingCatalog.ts) ----------

const SPECIAL = { æ: "ae", ø: "oe", å: "aa", œ: "oe", ß: "ss", ð: "d", þ: "th", ł: "l" };
function fold(text) {
  return String(text)
    .toLowerCase()
    .replace(/[æøåœßðþł]/g, (c) => SPECIAL[c] ?? c)
    .normalize("NFD")
    .replace(/\p{M}+/gu, "")
    .replace(/[^\p{L}\p{N}]+/gu, " ")
    .trim();
}
// ---------- Item objects ----------

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/** @type {Map<string, {id, kind: "custom", names, aliases: string[], section, icon, needsTranslation: boolean}>} */
const customs = new Map();

function newCustomId() {
  let id = "u_";
  for (let i = 0; i < 26; i++) id += CROCKFORD[Math.floor(Math.random() * 32)];
  return id;
}

function catalogObject(cat) {
  const names = { it: cat.names?.it ?? cat.name, en: cat.names?.en ?? cat.name, da: cat.names?.da ?? cat.name };
  return { id: cat.id, kind: "catalog", names, section: cat.section ?? null, icon: cat.icon ?? null };
}

function objectFor(id) {
  if (!id) return null;
  if (customs.has(id)) return customs.get(id);
  const cat = catalogItem(id);
  return cat ? catalogObject(cat) : null;
}

/** Typed text in any language to an object id (catalog names and aliases, then custom objects). */
function resolveText(text) {
  const q = fold(text);
  if (!q) return null;
  for (const cat of catalogFor("it").items) {
    if (!cat) continue;
    const terms = [cat.name, ...Object.values(cat.names ?? {}), ...(Array.isArray(cat.aliases) ? cat.aliases : [])];
    if (terms.some((t) => typeof t === "string" && fold(t) === q)) return cat.id;
  }
  for (const o of customs.values()) {
    if ([...Object.values(o.names), ...o.aliases].some((t) => fold(t) === q)) return o.id;
  }
  return null;
}

function createCustom(text) {
  const name = clip(text);
  const o = { id: newCustomId(), kind: "custom", names: { it: name, en: name, da: name }, aliases: [name], section: "other", icon: null, needsTranslation: true };
  customs.set(o.id, o);
  return o;
}

// Same rule as sameRow() in src/lib/shoppingOps.ts: one active row per object.
function sameRow(item, objectId, name) {
  if (objectId && item.objectId === objectId) return true;
  if (objectId && item.objectId) return false;
  return name !== "" && fold(item.name) === fold(name);
}

// ---------- Lists ----------

/** @type {Map<string, {id, name, theme, updatedAt, revision, items: any[], seen: Set<string>}>} */
const lists = new Map();
let down = false;

function newList(id, name, theme) {
  const list = { id, name, theme, updatedAt: iso(), revision: 1, items: [], seen: new Set() };
  lists.set(id, list);
  return list;
}

function seedItem(list, sampleId, spec, status, updatedBy, minutesAgo) {
  // Prefer the real catalog's entry; a sample id it lacks becomes a custom item.
  const real = catalogItem(sampleId);
  const cat = real ?? sampleCatalog("it").items.find((i) => i.id === sampleId);
  const at = new Date(Date.now() - minutesAgo * 60_000).toISOString();
  const objectId = real ? sampleId : createCustom(cat?.name ?? sampleId).id;
  if (!real && cat) Object.assign(customs.get(objectId), { names: { it: cat.names.it, en: cat.names.en, da: cat.names.da }, section: cat.section, icon: cat.icon, needsTranslation: false });
  list.items.push({
    id: randomUUID(),
    objectId,
    catalogId: real ? sampleId : null,
    name: cat?.name ?? sampleId,
    typedName: cat?.name ?? sampleId,
    spec,
    quantity: null,
    unit: null,
    section: cat?.section ?? null,
    icon: cat?.icon ?? null,
    status,
    position: list.items.length + 1,
    updatedAt: at,
    updatedBy,
    rev: 1,
  });
}

function seed() {
  const casa = newList(DEFAULT_LIST, "Casa", "home");
  seedItem(casa, "oat-milk", "x2", "needed", "telegram", 90);
  seedItem(casa, "eggs", "x6, bio", "needed", "web", 300);
  seedItem(casa, "coffee", "in grani", "needed", "lucina", 45);
  seedItem(casa, "banana", "", "needed", "ios", 12);
  seedItem(casa, "bread", "", "recent", "telegram", 1500);
  seedItem(casa, "basil", "", "recent", "discord", 2900);
  seedItem(casa, "parmesan", "24 mesi", "recent", "web", 4400);
  const candles = createCustom("Candele profumate");
  Object.assign(candles, { names: { it: "Candele profumate", en: "Scented candles", da: "Duftlys" }, section: "household", icon: "🕯️", needsTranslation: false });
  casa.items.push({
    id: randomUUID(), objectId: candles.id, catalogId: null, name: "Candele profumate", typedName: "candele profumate", spec: "", quantity: 2, unit: "pz",
    section: null, icon: null, status: "needed", position: casa.items.length + 1, updatedAt: iso(),
    updatedBy: "alexa", rev: 1,
  });
  const ferr = newList("hardware", "Ferramenta", "hardware");
  seedItem(ferr, "screws", "4x30", "needed", "web", 600);
  seedItem(ferr, "batteries", "AA", "recent", "telegram", 8000);
}
seed();

/** Rows as the server sends them: the object, and `name` in `lang`. */
const visible = (list, lang = "it") =>
  list.items
    .filter((i) => i.status !== "removed")
    .map(({ rev: _rev, objectId, ...rest }) => {
      const item = objectFor(objectId);
      return { ...rest, itemId: objectId, item, name: item?.names?.[lang] ?? rest.name, section: rest.section ?? item?.section ?? null, icon: rest.icon ?? item?.icon ?? null };
    });

function summary(list) {
  const active = list.items.filter((i) => i.status !== "removed");
  return {
    id: list.id,
    name: list.name,
    theme: list.theme,
    needed: active.filter((i) => i.status === "needed").length,
    recent: active.filter((i) => i.status === "recent").length,
    updatedAt: list.updatedAt,
  };
}

const listState = (list, lang) => ({
  list: { id: list.id, name: list.name, theme: list.theme },
  revision: list.revision,
  lang,
  items: visible(list, lang),
});

// ---------- Ops (same semantics as src/lib/shoppingOps.ts) ----------

const clip = (s) => String(s ?? "").trim().slice(0, 200);
const qty = (q) => (typeof q === "number" && Number.isFinite(q) && q > 0 ? q : null);
const nextPos = (list) => list.items.reduce((m, i) => (i.status === "needed" && i.position > m ? i.position : m), 0) + 1;
const active = (list, id) => list.items.find((i) => i.id === id && i.status !== "removed");
const TARGETED = new Set(["need", "purchase", "remove", "set_spec", "set_quantity", "set_section", "rename", "move", "reorder"]);
const TYPES = new Set([...TARGETED, "add", "clear_recent", "edit_item"]);

function validateOp(op) {
  if (!op || typeof op !== "object") return "op must be an object";
  if (typeof op.opId !== "string" || !op.opId || op.opId.length > 100) return "opId must be a non-empty string";
  if (!TYPES.has(op.type)) return `unknown op type ${JSON.stringify(op.type)}`;
  if (TARGETED.has(op.type) && typeof op.itemId !== "string") return `${op.type} needs itemId`;
  if (op.type === "add" && !clip(op.text ?? op.name) && !op.catalogId && !(op.itemId && !UUID_RE.test(op.itemId))) {
    return "add needs itemId or text";
  }
  if (op.type === "edit_item" && !customs.has(op.itemId)) return "edit_item needs a custom object id";
  if (op.type === "move" && (typeof op.toListId !== "string" || !lists.has(op.toListId))) return "move needs an existing toListId";
  return null;
}

function addToList(list, op, by, rev) {
  const text = clip(op.text ?? op.name);
  const rowId = op.rowId ?? (op.itemId && UUID_RE.test(op.itemId) ? op.itemId : undefined);
  const explicit = (op.itemId && !UUID_RE.test(op.itemId) ? op.itemId : undefined) ?? op.catalogId;
  const objectId = (explicit && objectFor(explicit) ? explicit : null) ?? resolveText(text) ?? createCustom(text).id;
  const object = objectFor(objectId);
  const cat = object?.kind === "catalog" ? object : null;
  const name = object?.names?.it ?? text;
  if (object?.kind === "custom" && text && !object.aliases.some((a) => fold(a) === fold(text))) object.aliases.push(text);
  const existing =
    (rowId ? active(list, rowId) : undefined) ??
    list.items.find((i) => i.status !== "removed" && sameRow(i, objectId, name));
  const touch = (i) => Object.assign(i, { updatedAt: iso(), updatedBy: by, rev });
  if (existing) {
    if (existing.status === "recent") {
      existing.position = nextPos(list);
      existing.status = "needed";
    }
    if (op.spec !== undefined) existing.spec = clip(op.spec);
    if (op.quantity !== undefined) existing.quantity = qty(op.quantity);
    if (op.unit !== undefined) existing.unit = op.unit ? clip(op.unit) : null;
    if (op.section !== undefined) existing.section = op.section;
    touch(existing);
    return existing;
  }
  const id = rowId && !list.items.some((i) => i.id === rowId) ? rowId : randomUUID();
  const item = {
    id,
    objectId,
    catalogId: cat ? objectId : null,
    name,
    typedName: text || name,
    spec: op.spec ? clip(op.spec) : "",
    quantity: qty(op.quantity),
    unit: op.unit ? clip(op.unit) : null,
    section: op.section ?? null,
    icon: null,
    status: "needed",
    position: nextPos(list),
    updatedAt: iso(),
    updatedBy: by,
    rev,
  };
  list.items.push(item);
  return item;
}

function applyOne(list, op, by, rev, lang) {
  const touch = (i) => Object.assign(i, { updatedAt: iso(), updatedBy: by, rev });
  if (op.type === "add") return void addToList(list, op, by, rev);
  if (op.type === "edit_item") {
    const o = customs.get(op.itemId);
    if (op.names) for (const [l, v] of Object.entries(op.names)) if (clip(v)) o.names[l] = clip(v);
    if (op.names) o.needsTranslation = false;
    if (typeof op.section === "string") o.section = op.section;
    if (typeof op.icon === "string") o.icon = op.icon || null;
    for (const l of lists.values()) for (const i of l.items) if (i.objectId === o.id && i.status !== "removed") touch(i);
    return;
  }
  if (op.type === "clear_recent") {
    for (const i of list.items) if (i.status === "recent") touch(Object.assign(i, { status: "removed" }));
    return;
  }
  const item = active(list, op.itemId);
  if (!item) return; // deleted stays deleted; unknown ids are no-ops
  switch (op.type) {
    case "need":
      if (item.status !== "needed") touch(Object.assign(item, { status: "needed", position: nextPos(list) }));
      break;
    case "purchase":
      if (item.status !== "recent") touch(Object.assign(item, { status: "recent" }));
      break;
    case "remove":
      touch(Object.assign(item, { status: "removed" }));
      break;
    case "set_spec":
      touch(Object.assign(item, { spec: clip(op.spec) }));
      break;
    case "set_quantity":
      item.quantity = qty(op.quantity);
      if (op.unit !== undefined) item.unit = op.unit ? clip(op.unit) : null;
      touch(item);
      break;
    case "set_section":
      touch(Object.assign(item, { section: typeof op.section === "string" ? op.section : null }));
      break;
    case "rename": {
      const name = clip(op.name);
      if (!name) break;
      const target = resolveText(name);
      const own = customs.get(item.objectId);
      if (!target && own) own.names[lang] = name;
      else Object.assign(item, { objectId: target ?? createCustom(name).id, catalogId: target && !customs.has(target) ? target : null, name, typedName: name, section: null, icon: null });
      touch(item);
      break;
    }
    case "reorder":
      if (Number.isFinite(op.position)) touch(Object.assign(item, { position: op.position }));
      break;
    case "move": {
      if (op.toListId === list.id) break;
      const target = lists.get(op.toListId);
      touch(Object.assign(item, { status: "removed" }));
      target.revision += 1;
      target.updatedAt = iso();
      addToList(
        target,
        { itemId: item.objectId, text: item.typedName, spec: item.spec, quantity: item.quantity, unit: item.unit, section: item.section },
        by,
        target.revision,
      );
      break;
    }
  }
}

function sourceOf(req) {
  const c = String(req.headers["x-secretary-client"] ?? "").toLowerCase();
  return SOURCES.includes(c) ? c : "web";
}

async function handleOps(req, res, ctx, list) {
  const body = await ctx.readJson(req);
  const lang = ["it", "en", "da"].includes(body.lang) ? body.lang : ["it", "en", "da"].includes(ctx.url.searchParams.get("lang")) ? ctx.url.searchParams.get("lang") : "it";
  const ops = body.ops;
  if (!Array.isArray(ops) || ops.length === 0 || ops.length > MAX_OPS) {
    return ctx.fail(res, 400, "bad_request", `ops must be an array of 1-${MAX_OPS} ops`);
  }
  for (const op of ops) {
    const problem = validateOp(op);
    if (problem) return ctx.fail(res, 422, "invalid_op", problem);
  }
  const expected = Number.isInteger(body.expectedRevision) ? body.expectedRevision : list.revision;
  await sleep(120);
  // 409 only when the revision is stale *and* a new op touches a row changed since then.
  if (expected < list.revision) {
    const conflict = ops.find((op) => {
      if (list.seen.has(op.opId) || !TARGETED.has(op.type)) return false;
      const item = list.items.find((i) => i.id === op.itemId);
      return item !== undefined && item.rev > expected;
    });
    if (conflict) {
      return ctx.send(res, 409, {
        error: { code: "revision_conflict", message: "The list changed. Refetch and retry." },
        revision: list.revision,
        conflictOpId: conflict.opId,
      });
    }
  }
  const by = sourceOf(req);
  const applied = [];
  const duplicates = [];
  let rev = list.revision;
  for (const op of ops) {
    if (list.seen.has(op.opId)) {
      duplicates.push(op.opId);
      continue;
    }
    if (applied.length === 0) rev = list.revision + 1;
    list.seen.add(op.opId);
    applyOne(list, op, by, rev, lang);
    applied.push(op.opId);
  }
  if (applied.length) {
    list.revision = rev;
    list.updatedAt = iso();
  }
  return ctx.send(res, 200, { revision: list.revision, applied, duplicates, lang, items: visible(list, lang) });
}

/**
 * Handles /v1/shopping/catalog, /v1/shopping/lists… and /v1/shopping/_mock/….
 * Returns false for anything else (the legacy /v1/shopping routes stay in server.mjs).
 * ctx = { path, method, url, send, fail, readJson }
 */
export async function handleShoppingV2(req, res, ctx) {
  const { path, method, url, send, fail, readJson } = ctx;
  if (!path.startsWith("/v1/shopping/")) return false;

  if (path === "/v1/shopping/_mock/down" && method === "POST") {
    down = (await readJson(req)).down !== false;
    send(res, 200, { down });
    return true;
  }
  if (path === "/v1/shopping/_mock/bump" && method === "POST") {
    const body = await readJson(req);
    const list = lists.get(typeof body.listId === "string" ? body.listId : DEFAULT_LIST);
    if (!list) return fail(res, 404, "list_not_found", "No such list."), true;
    list.revision += 1;
    const item = list.items.find((i) => i.status === "needed");
    if (item) Object.assign(item, { rev: list.revision, updatedAt: iso(), updatedBy: "telegram" });
    send(res, 200, { revision: list.revision, touched: item?.id ?? null });
    return true;
  }
  if (path === "/v1/shopping/_mock/external" && method === "POST") {
    const body = await readJson(req);
    const list = lists.get(typeof body.listId === "string" ? body.listId : DEFAULT_LIST);
    if (!list) return fail(res, 404, "list_not_found", "No such list."), true;
    const name = clip(body.name);
    if (!name) return fail(res, 400, "bad_request", "name required"), true;
    const by = SOURCES.includes(body.via) ? body.via : "telegram";
    const cat = catalogFor("it").items.find((i) => fold(i.name) === fold(name) || (i.aliases ?? []).some((a) => fold(a) === fold(name)));
    list.revision += 1;
    const item = addToList(list, { itemId: cat?.id, text: name, spec: body.spec }, by, list.revision);
    send(res, 201, item);
    return true;
  }

  if (down) {
    fail(res, 503, "lucina_unreachable", "Lucina is offline (mock).");
    return true;
  }

  if (path === "/v1/shopping/catalog") {
    if (method !== "GET") return fail(res, 405, "method_not_allowed", "Method not allowed."), true;
    const lang = ["it", "en", "da"].includes(url.searchParams.get("lang")) ? url.searchParams.get("lang") : "it";
    send(res, 200, catalogFor(lang), { "Cache-Control": "public, max-age=300" });
    return true;
  }

  if (path === "/v1/shopping/items") {
    if (method !== "GET") return fail(res, 405, "method_not_allowed", "Method not allowed."), true;
    send(res, 200, [...customs.values()]);
    return true;
  }

  if (path === "/v1/shopping/lists") {
    if (method === "GET") {
      await sleep(80);
      send(res, 200, [...lists.values()].map(summary));
      return true;
    }
    if (method === "POST") {
      const body = await readJson(req);
      const name = clip(body.name).slice(0, 80);
      if (!name) return fail(res, 400, "bad_request", "name must be 1-80 characters"), true;
      const theme = THEMES.includes(body.theme) ? body.theme : "other";
      const list = newList(randomUUID(), name, theme);
      send(res, 201, summary(list));
      return true;
    }
    fail(res, 405, "method_not_allowed", "Method not allowed.");
    return true;
  }

  const opsMatch = path.match(/^\/v1\/shopping\/lists\/([^/]+)\/ops$/);
  if (opsMatch) {
    const list = lists.get(decodeURIComponent(opsMatch[1]));
    if (!list) return fail(res, 404, "list_not_found", "No such list."), true;
    if (method !== "POST") return fail(res, 405, "method_not_allowed", "Method not allowed."), true;
    await handleOps(req, res, ctx, list);
    return true;
  }

  const listMatch = path.match(/^\/v1\/shopping\/lists\/([^/]+)$/);
  if (listMatch) {
    const id = decodeURIComponent(listMatch[1]);
    const list = lists.get(id);
    if (!list) return fail(res, 404, "list_not_found", "No such list."), true;
    if (method === "GET") {
      await sleep(100);
      const lang = ["it", "en", "da"].includes(url.searchParams.get("lang")) ? url.searchParams.get("lang") : "it";
      send(res, 200, listState(list, lang));
      return true;
    }
    if (method === "PATCH") {
      const body = await readJson(req);
      if (body.name !== undefined) {
        const name = clip(body.name).slice(0, 80);
        if (!name) return fail(res, 400, "bad_request", "name must be 1-80 characters"), true;
        list.name = name;
      }
      if (body.theme !== undefined) {
        if (!THEMES.includes(body.theme)) return fail(res, 400, "bad_request", "unknown theme"), true;
        list.theme = body.theme;
      }
      list.updatedAt = iso();
      send(res, 200, summary(list));
      return true;
    }
    if (method === "DELETE") {
      if (id === DEFAULT_LIST) return fail(res, 409, "default_list", "The default list can't be deleted."), true;
      lists.delete(id);
      send(res, 200, { deleted: true });
      return true;
    }
    fail(res, 405, "method_not_allowed", "Method not allowed.");
    return true;
  }

  return false;
}
