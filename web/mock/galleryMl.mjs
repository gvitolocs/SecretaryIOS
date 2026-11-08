// The albums API and smart search for the synthetic Gallery (mock/gallery.mjs calls this before its own item routes).
//
//   MOCK_SMART=off            smart search switched off: status says so, search and similar answer 503
//   MOCK_SMART_INDEXED=0.7    the share of the photos the index has "looked at" so far (1 = all, no hint in the UI)
//
// Albums (docs/media-library.md): GET/POST albums, GET/PATCH/DELETE albums/{id}, POST/DELETE albums/{id}/items. The
// listing's `scope=gallery` leaves out empty albums, as the server does. The mock also lists one hidden share album,
// which the real server never does, so the web client's filter has something to hide.
// Smart search: GET embeddings/status, GET items/search, GET items/{id}/similar. Scores are deterministic: a hash of
// each query word and the item id (plus a boost for a word in the title, place or caption) for a search, and the
// distance between the two pictures' colours for "similar". The order is by score, not by date.

const PAGE_DEFAULT = 60;
const PAGE_MAX = 200;

function fnv(text) {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h >>> 0;
}

const unit = (text) => fnv(text) / 4294967296;
const members = (items, id) => items.filter((i) => !i.trashed_at && !i.hidden && i.albums.includes(id));

function albumOut(items, a) {
  const list = members(items, a.id);
  const cover = a.cover === "auto" ? (list[0]?.id ?? null) : (a.cover ?? null);
  return { id: a.id, title: a.title, system_key: a.system_key, cover_item: cover, count: list.length, created_at: a.created_at, updated_at: a.updated_at };
}

function pageOf(p) {
  const limit = Math.min(PAGE_MAX, Math.max(1, Number(p.get("limit")) || PAGE_DEFAULT));
  let offset = 0;
  const cursor = p.get("cursor");
  if (cursor) {
    try {
      offset = Number(Buffer.from(cursor, "base64url").toString("utf8").replace(/^o:/, ""));
    } catch {
      offset = NaN;
    }
    if (!Number.isInteger(offset) || offset < 0) return null;
  }
  return { limit, offset };
}

/** Returns true when the request was answered. */
export async function handleGalleryMl(req, res, { rest, method, url, send, fail, readJson }, env) {
  const p = url.searchParams;
  const items = env.items();
  const albums = env.albums;
  const now = () => new Date().toISOString();
  let m;

  // ---- albums -----------------------------------------------------------------------------------------------------

  if (rest === "albums" && method === "GET") {
    const gallery = p.get("scope") === "gallery";
    const list = albums.map((a) => albumOut(items, a)).filter((a) => !gallery || a.count > 0);
    list.sort((a, b) => (a.system_key === null) === (b.system_key === null) ? (a.updated_at < b.updated_at ? 1 : -1) : a.system_key === null ? -1 : 1);
    return send(res, 200, { albums: list }), true;
  }
  if (rest === "albums" && method === "POST") {
    const body = await readJson(req);
    const title = typeof body.title === "string" ? body.title.trim() : "";
    if (!title || title.length > 200) return fail(res, 400, "bad_request", "title must be 1-200 characters"), true;
    const album = { id: `alb_${albums.length + 1}_${Date.now().toString(36)}`, title, system_key: null, cover: "auto", created_at: now(), updated_at: now() };
    albums.push(album);
    const known = new Map(items.map((i) => [i.id, i]));
    for (const id of Array.isArray(body.item_ids) ? body.item_ids : []) known.get(id)?.albums.push(album.id);
    return send(res, 201, albumOut(items, album)), true;
  }
  if ((m = /^albums\/([^/]+)$/.exec(rest))) {
    const album = albums.find((a) => a.id === decodeURIComponent(m[1]));
    if (!album) return fail(res, 404, "not_found", "not found"), true;
    if (method === "GET") return send(res, 200, albumOut(items, album)), true;
    if (method === "PATCH") {
      const body = await readJson(req);
      if (typeof body.title === "string") {
        const title = body.title.trim();
        if (!title || title.length > 200) return fail(res, 400, "bad_request", "title must be 1-200 characters"), true;
        album.title = title;
      }
      if (typeof body.cover_item === "string") album.cover = body.cover_item;
      album.updated_at = now();
      return send(res, 200, albumOut(items, album)), true;
    }
    if (method === "DELETE") {
      if (album.system_key !== null) return fail(res, 404, "not_found", "no such user album"), true;
      albums.splice(albums.indexOf(album), 1);
      for (const it of items) it.albums = it.albums.filter((a) => a !== album.id);
      res.statusCode = 204;
      return res.end(), true;
    }
  }
  if ((m = /^albums\/([^/]+)\/items$/.exec(rest)) && (method === "POST" || method === "DELETE")) {
    const album = albums.find((a) => a.id === decodeURIComponent(m[1]));
    if (!album) return fail(res, 404, "not_found", "not found"), true;
    const body = await readJson(req);
    const ids = new Set(Array.isArray(body.item_ids) ? body.item_ids : []);
    let changed = 0;
    for (const it of items) {
      if (!ids.has(it.id)) continue;
      if (method === "POST" && !it.albums.includes(album.id)) (it.albums.push(album.id), changed++);
      if (method === "DELETE" && it.albums.includes(album.id)) (it.albums = it.albums.filter((a) => a !== album.id), changed++);
    }
    album.updated_at = now();
    return send(res, 200, method === "POST" ? { added: changed } : { removed: changed }), true;
  }

  // ---- smart search ----------------------------------------------------------------------------------------------

  const off = process.env.MOCK_SMART === "off";
  const photos = () => items.filter((i) => i.kind === "photo" && !i.hidden);
  const indexedSet = () => {
    const all = photos();
    const share = Math.min(1, Math.max(0, Number(process.env.MOCK_SMART_INDEXED ?? 0.7)));
    return new Set(all.slice(0, Math.floor(all.length * share)).map((i) => i.id));
  };
  const status = () => {
    if (off) return { enabled: false, model: null, indexed: 0, pending: 0, running: false, paused_hot: false };
    const total = photos().length;
    const indexed = indexedSet().size;
    return { enabled: true, model: "mock-clip-vit-b32", indexed, pending: total - indexed, running: indexed < total, paused_hot: false };
  };

  if (rest === "embeddings/status" && method === "GET") return send(res, 200, status()), true;

  const similar = /^items\/([^/]+)\/similar$/.exec(rest);
  if ((rest === "items/search" || similar) && method === "GET") {
    if (off) return fail(res, 503, "smart_search_off", "smart search is not enabled"), true;
    const paging = pageOf(p);
    if (!paging) return fail(res, 400, "bad_cursor", "invalid cursor"), true;
    const filters = new URLSearchParams(p);
    filters.delete("q");
    const keep = env.matcher(filters);
    const indexed = indexedSet();
    let scored;
    if (similar) {
      const from = items.find((i) => i.id === decodeURIComponent(similar[1]));
      if (!from) return fail(res, 404, "not_found", "no such item"), true;
      scored = items
        .filter((i) => i.id !== from.id && indexed.has(i.id) && keep(i))
        .map((i) => {
          const d = Math.abs(i.hue - from.hue);
          return { it: i, score: Math.max(0, 1 - Math.min(d, 360 - d) / 180) * 0.97 + unit(i.id) * 0.03 };
        });
    } else {
      const words = (p.get("q") ?? "").toLowerCase().split(/\s+/).filter(Boolean);
      if (!words.length) return fail(res, 400, "bad_request", "q is required"), true;
      scored = items
        .filter((i) => indexed.has(i.id) && keep(i))
        .map((i) => {
          const text = `${i.title} ${i.caption ?? ""} ${i.place?.name ?? ""} ${i.device_name ?? ""}`.toLowerCase();
          const base = words.reduce((sum, w) => sum + unit(`${w}|${i.id}`), 0) / words.length;
          const boost = words.some((w) => text.includes(w)) ? 0.4 : 0;
          return { it: i, score: Math.min(0.99, 0.2 + base * 0.38 + boost) };
        });
    }
    scored.sort((a, b) => b.score - a.score || (a.it.id < b.it.id ? -1 : 1));
    const page = scored.slice(paging.offset, paging.offset + paging.limit);
    const next = paging.offset + paging.limit < scored.length ? Buffer.from(`o:${paging.offset + paging.limit}`).toString("base64url") : null;
    return send(res, 200, { items: page.map((x) => env.out(x.it)), scores: page.map((x) => Math.round(x.score * 1000) / 1000), next_cursor: next, status: status() }), true;
  }

  return false;
}
