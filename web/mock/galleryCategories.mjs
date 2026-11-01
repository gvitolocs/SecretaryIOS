// Automatic photo categories for the synthetic Gallery (mock/gallery.mjs calls this before its own item routes).
//
//   MOCK_CATEGORIES=off          the server's categories are off: no automatic albums, status says so
//   MOCK_CATEGORIES_DECIDED=0.8  the share of the photos decided so far (default 1; the rest have `category: null`)
//
// GET categories (the automatic albums with a count and the newest photo as cover), GET categories/{cat}/items (the
// listing's own filters and paging), GET categories/status, GET categories/examples, PUT items/{id}/category (the owner's
// label; `null` takes it back), and the `category` in GET items/{id}/details (`categoryOf`). A photo's category is a hash of
// its id, so it is the same on every start; the owner's labels live in memory until a restart.

const ORDER = ["screenshot", "receipt", "document", "note", "ticket", "map", "meme", "food", "pet", "photo"];
// Cumulative shares of the photos; the rest are plain photos.
const SHARES = [["screenshot", 0.08], ["receipt", 0.12], ["document", 0.15], ["note", 0.18], ["ticket", 0.19], ["map", 0.2], ["meme", 0.23], ["food", 0.27], ["pet", 0.32]];

function fnv(text) {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h >>> 0;
}

const unit = (text) => fnv(text) / 4294967296;
const owner = new Map(); // item id -> { category, auto, at }

function decidedShare() {
  const v = Number(process.env.MOCK_CATEGORIES_DECIDED ?? 1);
  return Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 1;
}

const off = () => process.env.MOCK_CATEGORIES === "off";

function autoCategory(it) {
  if (it.kind !== "photo" || off()) return null;
  if (unit(`decided|${it.id}`) >= decidedShare()) return null;
  const r = unit(`category|${it.id}`);
  return SHARES.find(([, upTo]) => r < upTo)?.[0] ?? "photo";
}

/** The `category` of an item as the server shows it, or null while it has none. */
export function categoryOf(it) {
  const own = owner.get(it.id);
  if (own) return { category: own.category, confidence: 1, decided_by: "owner", auto_category: own.auto };
  const category = autoCategory(it);
  return category ? { category, confidence: Math.round((0.6 + unit(`conf|${it.id}`) * 0.4) * 1000) / 1000, decided_by: "rules", auto_category: null } : null;
}

const effective = (it) => categoryOf(it)?.category ?? null;

function decode(cursor, list) {
  if (!cursor) return 0;
  let text;
  try {
    text = Buffer.from(cursor, "base64url").toString("utf8");
  } catch {
    return null;
  }
  const bar = text.lastIndexOf("|");
  if (bar < 0) return null;
  const at = text.slice(0, bar);
  const id = text.slice(bar + 1);
  const i = list.findIndex((it) => it.sort_at < at || (it.sort_at === at && it.id < id));
  return i < 0 ? list.length : i;
}

/** Returns true when the request was answered. */
export async function handleGalleryCategories(req, res, { rest, method, url, send, fail, readJson }, env) {
  const p = url.searchParams;
  const items = env.items();
  let m;

  if (rest === "categories" && method === "GET") {
    const keep = env.matcher(p);
    const counts = new Map();
    for (const it of items) {
      if (!keep(it)) continue;
      const c = effective(it);
      if (!c || c === "photo") continue;
      const entry = counts.get(c) ?? { count: 0, cover: it };
      entry.count++;
      if (it.sort_at > entry.cover.sort_at) entry.cover = it;
      counts.set(c, entry);
    }
    const categories = ORDER.filter((c) => counts.has(c)).map((c) => ({ category: c, count: counts.get(c).count, cover: env.out(counts.get(c).cover) }));
    return send(res, 200, { ok: true, categories }), true;
  }

  if (rest === "categories/status" && method === "GET") {
    const photos = items.filter((i) => i.kind === "photo" && !i.hidden && !i.trashed_at);
    const decided = photos.filter((i) => categoryOf(i)).length;
    const ownerCount = photos.filter((i) => owner.has(i.id)).length;
    return send(res, 200, {
      ok: true,
      enabled: !off(),
      model: off() ? null : "mock-clip-vit-b32",
      features_version: 1,
      decider: { kind: "rules", id: "rules-1" },
      decided: off() ? 0 : decided,
      pending: off() ? 0 : photos.length - decided,
      stale: 0,
      by_decider: { rules: decided - ownerCount, jev: 0, owner: ownerCount },
      running: !off() && decided < photos.length,
      paused_hot: false,
      paused_busy: false,
    }), true;
  }

  if (rest === "categories/examples" && method === "GET") {
    const examples = [...owner].map(([id, o]) => ({ item_id: id, category: o.category, auto_category: o.auto, features: null, model: "mock-clip-vit-b32", labelled_at: o.at }));
    return send(res, 200, { ok: true, examples, next_cursor: null }), true;
  }

  if ((m = /^categories\/([^/]+)\/items$/.exec(rest)) && method === "GET") {
    const cat = decodeURIComponent(m[1]);
    if (!ORDER.includes(cat)) return fail(res, 404, "not_found", "category not found"), true;
    const keep = env.matcher(p);
    const limit = Math.min(500, Math.max(1, Number(p.get("limit")) || 200));
    let i = decode(p.get("cursor"), items);
    if (i === null) return fail(res, 400, "bad_cursor", "invalid cursor"), true;
    const inCat = (it) => keep(it) && (effective(it) ?? "photo") === cat;
    const page = [];
    for (; i < items.length && page.length < limit; i++) if (inCat(items[i])) page.push(items[i]);
    const more = page.length === limit && items.slice(i).some(inCat);
    const last = page[page.length - 1];
    return send(res, 200, { ok: true, items: page.map(env.out), next_cursor: more ? Buffer.from(`${last.sort_at}|${last.id}`).toString("base64url") : null, total: null, albums: [] }), true;
  }

  if ((m = /^items\/([^/]+)\/category$/.exec(rest)) && method === "PUT") {
    const it = items.find((i) => i.id === decodeURIComponent(m[1]));
    if (!it) return fail(res, 404, "not_found", "item not found"), true;
    if (it.kind !== "photo") return fail(res, 400, "bad_request", "only photos have a category"), true;
    const body = await readJson(req);
    if (!("category" in body)) return fail(res, 400, "bad_request", "category is required"), true;
    if (body.category === null) {
      owner.delete(it.id);
      return send(res, 200, { ok: true, category: categoryOf(it) }), true;
    }
    if (typeof body.category !== "string" || !ORDER.includes(body.category)) return fail(res, 400, "bad_request", "unknown category"), true;
    const before = owner.get(it.id)?.auto ?? autoCategory(it);
    owner.set(it.id, { category: body.category, auto: before, at: new Date().toISOString() });
    return send(res, 200, { ok: true, category: categoryOf(it) }), true;
  }

  return false;
}
