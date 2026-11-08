// Faces and people for the synthetic Gallery (mock/gallery.mjs calls this before its own item routes).
//
//   MOCK_FACES=off              face recognition starts switched off (the People page offers "Turn on")
//   MOCK_FACES_SCANNED=0.6      the share of the photos the scan has "looked at" (1 = all, no progress bar)
//   MOCK_FACES_PAUSED=hot|busy  the scan is paused for that reason while photos are still waiting
//
// Routes (contract: faces/CONTRACT.md): GET/PUT faces/settings, DELETE faces, GET people, GET/PATCH people/{id},
// POST people/{id}/merge, GET people/{id}/items, GET people/{id}/thumb, GET faces/{id}/thumb, GET items/{id}/faces,
// PUT faces/{id}, GET faces/unassigned. The photos' own pictures show the faces (see `facesSvg`), details carry `people`
// (see `peopleOf`) and the listing filter `person=` is `hasPerson`. Everything is deterministic from the item ids (people
// are invented names; no real photos or faces). Edits live in memory; a restart resets them. "Delete face data" empties
// everything, and turning recognition on again brings the synthetic scan back.

const PAGE_DEFAULT = 60;
const PAGE_MAX = 500;
const MIN_FACES = 3;

const SEED_PEOPLE = [
  { id: "per_alma", name: "Alma", hue: 12, weight: 6 },
  { id: "per_bruno", name: "Bruno", hue: 205, weight: 5 },
  { id: "per_chiara", name: "Chiara", hue: 130, weight: 4 },
  { id: "per_dario", name: "Dario", hue: 330, weight: 3, birth_date: "1990-04-12" },
  { id: "per_unnamed1", name: null, hue: 275, weight: 3 },
  { id: "per_unnamed2", name: null, hue: 55, weight: 2 },
  { id: "per_hidden", name: null, hue: 180, weight: 2, hidden: true },
  { id: "per_elena", name: "Elena", hue: 95, weight: 0 },
];

function fnv(text) {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h >>> 0;
}

const unit = (text) => fnv(text) / 4294967296;

let state = null;

function seed(items) {
  const people = SEED_PEOPLE.map((p) => ({ id: p.id, name: p.name, hidden: p.hidden === true, birth_date: p.birth_date ?? null, hue: p.hue, cover_face_id: null, version: 1 }));
  const total = SEED_PEOPLE.reduce((sum, p) => sum + p.weight, 0);
  const faces = [];
  for (const it of items) {
    if (it.kind !== "photo" || it.hidden || unit(`has|${it.id}`) > 0.3) continue;
    const count = 1 + Math.floor(unit(`n|${it.id}`) * 3);
    const aspect = it.height ? it.width / it.height : 1;
    for (let k = 0; k < count; k++) {
      const key = `${it.id}|${k}`;
      // A person by weight; about one face in ten is left without a person (the server wasn't sure).
      let pick = unit(`who|${key}`) * total;
      let who = SEED_PEOPLE[0];
      for (const p of SEED_PEOPLE) {
        pick -= p.weight;
        if (pick <= 0) {
          who = p;
          break;
        }
      }
      const unassigned = unit(`free|${key}`) < 0.1;
      const w = 0.12 + unit(`w|${key}`) * 0.05;
      const h = Math.min(0.5, w * aspect * 1.15);
      const x = Math.min(0.95 - w, 0.12 + (k / count) * 0.7 + unit(`x|${key}`) * 0.08);
      const y = Math.min(0.9 - h, 0.22 + unit(`y|${key}`) * 0.3);
      faces.push({
        id: `fc_${it.id.slice(4)}_${k}`,
        item_id: it.id,
        box: { x: Math.round(x * 1000) / 1000, y: Math.round(y * 1000) / 1000, w: Math.round(w * 1000) / 1000, h: Math.round(h * 1000) / 1000 },
        score: Math.round((0.86 + unit(`s|${key}`) * 0.13) * 1000) / 1000,
        source: "ml",
        person_id: unassigned ? null : who.id,
        hue: who.hue,
      });
    }
  }
  // Elena has only two faces: below the page's minimum, so she shows up only where the minimum is lowered (the picker).
  const elena = people.find((x) => x.id === "per_elena");
  for (const f of faces.filter((x) => x.person_id !== null).slice(0, 2)) {
    f.person_id = elena.id;
    f.hue = elena.hue;
  }
  const byPerson = new Map();
  for (const f of faces) if (f.person_id && !byPerson.has(f.person_id)) byPerson.set(f.person_id, f.id);
  for (const p of people) p.cover_face_id = byPerson.get(p.id) ?? null;
  return { enabled: process.env.MOCK_FACES !== "off", deleted: false, people, faces, byItem: indexByItem(faces) };
}

function indexByItem(faces) {
  const map = new Map();
  for (const f of faces) {
    const list = map.get(f.item_id);
    if (list) list.push(f);
    else map.set(f.item_id, [f]);
  }
  return map;
}

function ensure(items) {
  if (!state) state = seed(items);
  return state;
}

function stats(person) {
  let faceCount = 0;
  const seen = new Set();
  for (const f of state.faces) {
    if (f.person_id !== person.id) continue;
    faceCount++;
    seen.add(f.item_id);
  }
  return { faceCount, itemCount: seen.size };
}

function personOut(person) {
  const { faceCount, itemCount } = stats(person);
  return {
    id: person.id,
    name: person.name,
    hidden: person.hidden,
    birth_date: person.birth_date,
    face_count: faceCount,
    item_count: itemCount,
    cover_face_id: person.cover_face_id,
    thumb_url: `/v1/media/people/${person.id}/thumb?v=${person.version}`,
  };
}

const visible = (person) => !person.hidden && stats(person).faceCount >= MIN_FACES;

function faceOut(f) {
  const person = f.person_id ? state.people.find((p) => p.id === f.person_id) : null;
  return { id: f.id, box: f.box, score: f.score, source: f.source, person: person ? { id: person.id, name: person.name } : null };
}

/** The people on a photo for its details: the visible ones, named or not. */
export function peopleOf(itemId) {
  if (!state?.enabled) return [];
  const out = [];
  const seen = new Set();
  for (const f of state.byItem.get(itemId) ?? []) {
    const person = f.person_id ? state.people.find((p) => p.id === f.person_id) : null;
    if (!person || seen.has(person.id) || !visible(person)) continue;
    seen.add(person.id);
    out.push({ id: person.id, name: person.name });
  }
  return out;
}

/** The listing filter `person=<id>`: the photo has a face of that person. */
export function hasPerson(itemId, personId) {
  if (!state) return false;
  return (state.byItem.get(itemId) ?? []).some((f) => f.person_id === personId);
}

// ---- pictures -----------------------------------------------------------------------------------------------------

function faceShapes(cx, cy, rx, ry, hue) {
  const skin = `hsl(${24 + (hue % 18)} 48% ${66 - (hue % 5) * 3}%)`;
  const hair = `hsl(${hue} 35% 22%)`;
  const eye = rx * 0.22;
  return (
    `<ellipse cx="${cx}" cy="${cy - ry * 0.12}" rx="${rx * 1.06}" ry="${ry * 1.0}" fill="${hair}"/>` +
    `<ellipse cx="${cx}" cy="${cy + ry * 0.05}" rx="${rx}" ry="${ry}" fill="${skin}"/>` +
    `<circle cx="${cx - rx * 0.36}" cy="${cy - ry * 0.05}" r="${eye}" fill="#2b2118"/>` +
    `<circle cx="${cx + rx * 0.36}" cy="${cy - ry * 0.05}" r="${eye}" fill="#2b2118"/>` +
    `<path d="M${cx - rx * 0.38} ${cy + ry * 0.42} Q${cx} ${cy + ry * 0.7} ${cx + rx * 0.38} ${cy + ry * 0.42}" fill="none" stroke="#7a3b2e" stroke-width="${Math.max(1, rx * 0.12)}" stroke-linecap="round"/>`
  );
}

/** Drawn into a photo's SVG (mock/gallery.mjs `svgFor`): a simple face in every box. */
export function facesSvg(itemId, w, h) {
  if (!state) return "";
  return (state.byItem.get(itemId) ?? [])
    .map((f) => faceShapes((f.box.x + f.box.w / 2) * w, (f.box.y + f.box.h / 2) * h, (f.box.w / 2) * w * 0.9, (f.box.h / 2) * h * 0.95, f.hue))
    .join("");
}

function thumbSvg(hue, seedText) {
  const bg = `hsl(${(hue + 40) % 360} 40% ${28 + Math.floor(unit(seedText) * 14)}%)`;
  return `<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256"><rect width="256" height="256" fill="${bg}"/><path d="M20 256 Q128 150 236 256 Z" fill="hsl(${hue} 40% 36%)"/>${faceShapes(128, 118, 62, 74, hue)}</svg>`;
}

function image(res, svg) {
  res.writeHead(200, { "Content-Type": "image/svg+xml", "Cache-Control": "private, max-age=60" });
  res.end(svg);
}

// ---- the routes ---------------------------------------------------------------------------------------------------

function offsetOf(p) {
  const limit = Math.min(PAGE_MAX, Math.max(1, Number(p.get("limit")) || PAGE_DEFAULT));
  let offset = 0;
  const cursor = p.get("cursor");
  if (cursor) {
    offset = Number(Buffer.from(cursor, "base64url").toString("utf8").replace(/^o:/, ""));
    if (!Number.isInteger(offset) || offset < 0) return null;
  }
  return { limit, offset };
}

const cursorFor = (offset) => Buffer.from(`o:${offset}`).toString("base64url");

/** Returns true when the request was answered. */
export async function handleGalleryFaces(req, res, { rest, method, url, send, fail, readJson }, env) {
  const p = url.searchParams;
  const items = env.items();
  // Seeded by the first request that gets here, so the pictures (answered after this) already show the faces.
  ensure(items);
  let m;

  if (rest === "faces/settings") {
    const s = ensure(items);
    if (method === "PUT") {
      const body = await readJson(req);
      if (typeof body.enabled !== "boolean") return fail(res, 400, "bad_request", "enabled: true or false"), true;
      if (body.enabled && s.deleted) state = { ...seed(items), enabled: true };
      state.enabled = body.enabled;
    } else if (method !== "GET") return false;
    const now = state;
    const photos = items.filter((i) => i.kind === "photo" && !i.hidden).length;
    const share = Math.min(1, Math.max(0, Number(process.env.MOCK_FACES_SCANNED ?? 1)));
    const scanned = now.deleted ? 0 : Math.floor(photos * share);
    const pending = photos - scanned;
    const paused = pending > 0 && now.enabled ? process.env.MOCK_FACES_PAUSED : undefined;
    return send(res, 200, {
      enabled: now.enabled,
      model: "mock-yunet-sface",
      scanned,
      pending,
      faces: now.faces.length,
      people: now.people.filter(visible).length,
      running: now.enabled && pending > 0 && !paused,
      paused_hot: paused === "hot",
      paused_busy: paused === "busy",
    }), true;
  }

  if (rest === "faces" && method === "DELETE") {
    ensure(items);
    state = { enabled: false, deleted: true, people: [], faces: [], byItem: new Map() };
    return send(res, 200, { ok: true }), true;
  }

  if (rest === "faces/unassigned" && method === "GET") {
    const s = ensure(items);
    const paging = offsetOf(p);
    if (!paging) return fail(res, 400, "bad_cursor", "invalid cursor"), true;
    const loose = s.faces.filter((f) => f.person_id === null);
    const page = loose.slice(paging.offset, paging.offset + paging.limit);
    const next = paging.offset + paging.limit < loose.length ? cursorFor(paging.offset + paging.limit) : null;
    return send(res, 200, { faces: page.map((f) => ({ id: f.id, item_id: f.item_id, thumb_url: `/v1/media/faces/${f.id}/thumb`, score: f.score })), next_cursor: next }), true;
  }

  if ((m = /^faces\/([^/]+)\/thumb$/.exec(rest)) && method === "GET") {
    const s = ensure(items);
    const face = s.faces.find((f) => f.id === decodeURIComponent(m[1]));
    if (!face) return fail(res, 404, "not_found", "no such face"), true;
    return image(res, thumbSvg(face.hue, face.id)), true;
  }

  if ((m = /^faces\/([^/]+)$/.exec(rest)) && method === "PUT") {
    const s = ensure(items);
    const face = s.faces.find((f) => f.id === decodeURIComponent(m[1]));
    if (!face) return fail(res, 404, "not_found", "no such face"), true;
    const body = await readJson(req);
    if (typeof body.new_person === "string") {
      const name = body.new_person.trim();
      if (!name || name.length > 80) return fail(res, 400, "bad_request", "new_person: 1 to 80 characters"), true;
      const person = { id: `per_new${s.people.length + 1}_${Date.now().toString(36)}`, name, hidden: false, birth_date: null, hue: face.hue, cover_face_id: face.id, version: 1 };
      s.people.push(person);
      face.person_id = person.id;
    } else if (body.person_id === null) {
      face.person_id = null;
    } else if (typeof body.person_id === "string") {
      const person = s.people.find((x) => x.id === body.person_id);
      if (!person) return fail(res, 404, "not_found", "no such person"), true;
      face.person_id = person.id;
      face.hue = person.hue;
      person.cover_face_id ??= face.id;
    } else return fail(res, 400, "bad_request", "person_id or new_person"), true;
    face.source = "manual";
    return send(res, 200, faceOut(face)), true;
  }

  if ((m = /^items\/([^/]+)\/faces$/.exec(rest)) && method === "GET") {
    const s = ensure(items);
    const id = decodeURIComponent(m[1]);
    if (!items.some((i) => i.id === id)) return fail(res, 404, "not_found", "no such item"), true;
    if (!s.enabled) return send(res, 200, { faces: [] }), true;
    return send(res, 200, { faces: (s.byItem.get(id) ?? []).map(faceOut) }), true;
  }

  if (rest === "people" && method === "GET") {
    const s = ensure(items);
    if (!s.enabled) return send(res, 200, { people: [] }), true;
    const minFaces = Number(p.get("min_faces") ?? MIN_FACES);
    const hidden = p.get("include_hidden") === "true";
    const list = s.people
      .filter((x) => (hidden || !x.hidden) && stats(x).faceCount >= (Number.isFinite(minFaces) ? minFaces : MIN_FACES))
      .map(personOut);
    list.sort((a, b) => Number(b.name !== null) - Number(a.name !== null) || b.face_count - a.face_count || (a.id < b.id ? -1 : 1));
    return send(res, 200, { people: list }), true;
  }

  if ((m = /^people\/([^/]+)(?:\/(thumb|items|merge))?$/.exec(rest))) {
    const s = ensure(items);
    const person = s.people.find((x) => x.id === decodeURIComponent(m[1]));
    if (!person) return fail(res, 404, "not_found", "no such person"), true;
    const sub = m[2] ?? "";
    if (sub === "" && method === "GET") return send(res, 200, personOut(person)), true;
    if (sub === "" && method === "PATCH") {
      const body = await readJson(req);
      if ("name" in body) {
        const name = typeof body.name === "string" ? body.name.trim() : null;
        if (name && name.length > 80) return fail(res, 400, "bad_request", "name: at most 80 characters"), true;
        person.name = name || null;
      }
      if (typeof body.hidden === "boolean") person.hidden = body.hidden;
      if ("birth_date" in body) {
        if (body.birth_date !== null && !/^\d{4}-\d{2}-\d{2}$/.test(String(body.birth_date))) return fail(res, 400, "bad_request", "birth_date: YYYY-MM-DD"), true;
        person.birth_date = body.birth_date;
      }
      if (typeof body.cover_face_id === "string" && s.faces.some((f) => f.id === body.cover_face_id && f.person_id === person.id)) {
        person.cover_face_id = body.cover_face_id;
        person.version++;
      }
      return send(res, 200, personOut(person)), true;
    }
    if (sub === "thumb" && method === "GET") return image(res, thumbSvg(person.hue, person.id)), true;
    if (sub === "merge" && method === "POST") {
      const body = await readJson(req);
      const from = new Set(Array.isArray(body.from) ? body.from.filter((x) => typeof x === "string" && x !== person.id) : []);
      if (!from.size || ![...from].every((id) => s.people.some((x) => x.id === id))) return fail(res, 400, "bad_request", "from: existing person ids"), true;
      for (const f of s.faces) if (from.has(f.person_id)) f.person_id = person.id;
      s.people = s.people.filter((x) => !from.has(x.id));
      return send(res, 200, personOut(person)), true;
    }
    if (sub === "items" && method === "GET") {
      const paging = offsetOf(p);
      if (!paging) return fail(res, 400, "bad_cursor", "invalid cursor"), true;
      const keep = env.matcher(p);
      const all = items.filter((it) => keep(it) && hasPerson(it.id, person.id));
      const page = all.slice(paging.offset, paging.offset + paging.limit);
      const next = paging.offset + paging.limit < all.length ? cursorFor(paging.offset + paging.limit) : null;
      return send(res, 200, { items: page.map(env.out), next_cursor: next, total: all.length }), true;
    }
  }

  return false;
}
