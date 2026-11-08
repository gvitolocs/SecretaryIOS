// The duplicate review and stacks for the synthetic Gallery (mock/gallery.mjs calls this after the albums and smart search).
//
//   MOCK_DEDUP=empty        no duplicate groups and no burst proposals (the empty state)
//   MOCK_DEDUP_PENDING=120  photos whose perceptual hash is "not computed yet" (the pending note)
//
// Routes (docs: crates/lucina-media/src/{dedup_review.rs,stacks.rs}): GET duplicates, POST duplicates/{resolve,dismiss,undo,
// resolve-all}, GET/POST stacks, GET/PUT/DELETE stacks/{id}, GET stacks/suggestions, POST stacks/suggestions/dismiss.
// The groups are fixed sets of generated photos picked at the first request; resolving hides the copies (`hidden`), a stack
// hides all its members but the primary, exactly as the listing then shows them. A restart resets everything.

const MIN_SCORE = 0.7;
const LIMIT_MAX = 200;

const GROUP_DEFS = [
  { at: 30, count: 2, cls: "same_photo_reencoded", score: 0.94 },
  { at: 50, count: 3, cls: "resized_copy", score: 0.86 },
  { at: 70, count: 2, cls: "exact", score: 1 },
  { at: 90, count: 2, cls: "same_photo_reencoded", score: 0.62 },
];
const SUGGESTION_DEFS = [
  { at: 130, count: 4, kind: "burst", evidence: "time_and_look" },
  { at: 150, count: 2, kind: "raw_jpeg", evidence: "name" },
  { at: 170, count: 3, kind: "burst", evidence: "time" },
];

const state = { seeded: false, groups: [], suggestions: [], stacks: new Map(), merged: new Map(), dismissedPairs: new Set(), dismissedSets: new Set(), seq: 0 };

const pairKey = (a, b) => (a < b ? `${a}|${b}` : `${b}|${a}`);
const visible = (it) => it && !it.hidden && !it.trashed_at;

function metadataCount(it) {
  return [it.taken_at, it.place, it.camera, it.caption, it.albums.length > 0, it.favorite, it.width].filter(Boolean).length;
}

export function seedDedup(items) {
  if (state.seeded) return;
  state.seeded = true;
  const pool = items.filter((i) => i.kind === "photo" && !i.hidden && !i.trashed_at && !i.live_pair && !i.archived && i.device);
  const take = (at, count) => pool.slice(at, at + count);
  if (process.env.MOCK_DEDUP !== "empty") {
    GROUP_DEFS.forEach((def, n) => {
      const members = take(def.at, def.count);
      if (members.length < 2) return;
      members.slice(1).forEach((m, k) => {
        m.size = Math.round(members[0].size * (0.4 + 0.1 * k));
        if (def.cls === "resized_copy") {
          m.width = Math.round(members[0].width / (k + 2));
          m.height = Math.round(members[0].height / (k + 2));
        }
      });
      state.groups.push({ id: `dg_mock${n}`, cls: def.cls, score: def.score, ids: members.map((m) => m.id) });
    });
    SUGGESTION_DEFS.forEach((def, n) => {
      const members = take(def.at, def.count);
      if (members.length < 2) return;
      state.suggestions.push({ id: `sg_mock${n}`, kind: def.kind, evidence: def.evidence, ids: members.map((m) => m.id) });
    });
  }
  // One stack from the start, so the grid shows a count badge: a burst of four, the newest is the primary.
  const burst = take(110, 4);
  if (burst.length === 4) makeStack(items, burst.map((m) => m.id), burst[0].id, "burst");
}

function makeStack(items, ids, primary, kind) {
  const id = `stk_mock${++state.seq}`;
  state.stacks.set(id, { id, kind, primary, ids: [...ids], created_at: new Date().toISOString() });
  const byId = new Map(items.map((i) => [i.id, i]));
  for (const m of ids) {
    const it = byId.get(m);
    it.stack_id = id;
    it.hidden = m !== primary;
  }
  return state.stacks.get(id);
}

/** `stack` of an item's JSON: null for a single photo. */
export function stackRef(it) {
  const stack = it.stack_id ? state.stacks.get(it.stack_id) : null;
  return stack ? { id: stack.id, count: stack.ids.length, primary: stack.primary === it.id } : null;
}

function keeper(members) {
  const rank = (a, b) => b.size - a.size || metadataCount(b) - metadataCount(a) || (a.created_at < b.created_at ? -1 : 1);
  const sorted = [...members].sort(rank);
  const [best, next] = sorted;
  let reason = "first_added";
  if (best.size !== next.size) reason = "largest_file";
  else if (metadataCount(best) !== metadataCount(next)) reason = "most_metadata";
  else if (best.device_name !== next.device_name) reason = "device_original";
  return { best, sorted, reason };
}

function openGroups(items) {
  const byId = new Map(items.map((i) => [i.id, i]));
  const out = [];
  for (const g of state.groups) {
    const members = g.ids.map((id) => byId.get(id)).filter(visible);
    if (members.length < 2) continue;
    let pairs = 0;
    let dismissed = 0;
    for (let a = 0; a < members.length; a++) {
      for (let b = a + 1; b < members.length; b++) {
        pairs++;
        if (state.dismissedPairs.has(pairKey(members[a].id, members[b].id))) dismissed++;
      }
    }
    if (dismissed === pairs) continue;
    out.push({ g, members, ...keeper(members) });
  }
  out.sort((a, b) => Number(b.g.score >= MIN_SCORE) - Number(a.g.score >= MIN_SCORE) || b.g.score - a.g.score || (a.g.id < b.g.id ? -1 : 1));
  return out;
}

function openSuggestions(items) {
  const byId = new Map(items.map((i) => [i.id, i]));
  const out = [];
  for (const s of state.suggestions) {
    const members = s.ids.map((id) => byId.get(id)).filter((i) => visible(i) && !i.stack_id);
    if (members.length < 2 || state.dismissedSets.has([...s.ids].sort().join("|"))) continue;
    out.push({ s, members });
  }
  return out;
}

const groupOut = (e, out) => ({
  id: e.g.id,
  class: e.g.cls,
  score: e.g.score,
  applicable: e.g.score >= MIN_SCORE,
  keep: e.best.id,
  reason: e.reason,
  members: e.sorted.map((m) => ({ ...out(m), review: { metadata: metadataCount(m), keep: m.id === e.best.id } })),
});

const stackOut = (stack, items, out) => {
  const byId = new Map(items.map((i) => [i.id, i]));
  const rest = stack.ids.filter((id) => id !== stack.primary).map((id) => byId.get(id)).sort((a, b) => (a.sort_at < b.sort_at ? -1 : 1));
  return {
    id: stack.id,
    kind: stack.kind,
    primary: stack.primary,
    count: stack.ids.length,
    created_at: stack.created_at,
    items: [byId.get(stack.primary), ...rest].map(out),
  };
};

function merge(items, keepId, dropIds, byId) {
  const merged = [];
  const skipped = [];
  for (const id of dropIds) {
    const it = byId.get(id);
    if (!it) skipped.push({ id, reason: "missing" });
    else if (id === keepId) skipped.push({ id, reason: "same_item" });
    else if (it.trashed_at) skipped.push({ id, reason: "trashed" });
    else if (it.hidden) skipped.push({ id, reason: "hidden" });
    else if (it.stack_id) skipped.push({ id, reason: "stacked" });
    else {
      it.hidden = true;
      state.merged.set(id, keepId);
      merged.push({ id, into: keepId });
    }
  }
  return { merged, skipped };
}

const idList = (v, max) => Array.isArray(v) && v.length <= max && v.every((x) => typeof x === "string" && x) ? v : null;

/** Returns true when the request was answered. */
export async function handleGalleryDedup(req, res, { rest, method, url, send, fail, readJson }, env) {
  if (!/^(duplicates|stacks)(\/|$)/.test(rest)) return false;
  const items = env.items();
  seedDedup(items);
  const byId = new Map(items.map((i) => [i.id, i]));
  const out = env.out;
  let m;

  // ---- duplicates -------------------------------------------------------------------------------------------------

  if (rest === "duplicates" && method === "GET") {
    const p = url.searchParams;
    if ((p.get("status") ?? "open") !== "open") return fail(res, 400, "bad_request", "status must be open"), true;
    const limit = Math.min(LIMIT_MAX, Math.max(1, Number(p.get("limit")) || 50));
    const offset = Math.max(0, Number(p.get("offset")) || 0);
    const groups = openGroups(items);
    const sure = groups.filter((e) => e.g.score >= MIN_SCORE);
    const pending = Number(process.env.MOCK_DEDUP_PENDING ?? 0) || 0;
    const photos = items.filter((i) => i.kind === "photo" && visible(i)).length;
    return send(res, 200, {
      ok: true,
      status: "open",
      hashes: { photos, hashed: Math.max(0, photos - pending), pending },
      total: groups.length,
      applicable: sure.length,
      min_score: MIN_SCORE,
      reclaimable_bytes: sure.reduce((n, e) => n + e.sorted.filter((x) => x.id !== e.best.id).reduce((a, x) => a + x.size, 0), 0),
      stack_suggestions: openSuggestions(items).length,
      groups: groups.slice(offset, offset + limit).map((e) => groupOut(e, out)),
    }), true;
  }

  if (rest === "duplicates/resolve" && method === "POST") {
    const body = await readJson(req);
    const keep = idList(body.keep, 200);
    const drop = idList(body.drop, 200) ?? [];
    if (!keep?.length || keep.length + drop.length > 200 || drop.some((d) => keep.includes(d))) return fail(res, 400, "bad_request", "keep: at least one id; keep and drop must not overlap"), true;
    for (let a = 0; a < keep.length; a++) for (let b = a + 1; b < keep.length; b++) state.dismissedPairs.add(pairKey(keep[a], keep[b]));
    const result = merge(items, keep[0], drop, byId);
    return send(res, 200, { ok: true, ...result, metadata: { captions: 0, location: false, taken_at: false } }), true;
  }

  if (rest === "duplicates/dismiss" && method === "POST") {
    const body = await readJson(req);
    const ids = idList(body.ids, 50);
    if (!ids || ids.length < 2) return fail(res, 400, "bad_request", "ids: 2 to 50 ids"), true;
    let pairs = 0;
    for (let a = 0; a < ids.length; a++) {
      for (let b = a + 1; b < ids.length; b++) {
        pairs++;
        if (body.undo === true) state.dismissedPairs.delete(pairKey(ids[a], ids[b]));
        else state.dismissedPairs.add(pairKey(ids[a], ids[b]));
      }
    }
    return send(res, 200, { ok: true, pairs }), true;
  }

  if (rest === "duplicates/undo" && method === "POST") {
    const body = await readJson(req);
    const ids = idList(body.ids, 200);
    if (!ids?.length) return fail(res, 400, "bad_request", "ids: at least one id"), true;
    const undone = [];
    const notMerged = [];
    for (const id of ids) {
      if (state.merged.has(id)) {
        byId.get(id).hidden = false;
        state.merged.delete(id);
        undone.push(id);
      } else notMerged.push(id);
    }
    return send(res, 200, { ok: true, undone, not_merged: notMerged }), true;
  }

  if (rest === "duplicates/resolve-all" && method === "POST") {
    const body = await readJson(req);
    const min = Math.min(1, Math.max(MIN_SCORE, typeof body.min_score === "number" ? body.min_score : MIN_SCORE));
    const merged = [];
    const skipped = {};
    let groups = 0;
    for (const e of openGroups(items)) {
      if (e.g.score < min) continue;
      groups++;
      const result = merge(items, e.best.id, e.sorted.filter((x) => x.id !== e.best.id).map((x) => x.id), byId);
      merged.push(...result.merged);
      for (const s of result.skipped) skipped[s.reason] = (skipped[s.reason] ?? 0) + 1;
    }
    return send(res, 200, { ok: true, groups, merged: merged.length, merged_ids: merged, skipped }), true;
  }

  // ---- stacks -----------------------------------------------------------------------------------------------------

  if (rest === "stacks/suggestions" && method === "GET") {
    const limit = Math.min(LIMIT_MAX, Math.max(1, Number(url.searchParams.get("limit")) || 50));
    const list = openSuggestions(items);
    const pending = Number(process.env.MOCK_DEDUP_PENDING ?? 0) || 0;
    return send(res, 200, {
      ok: true,
      hashes_pending: pending,
      total: list.length,
      // The generated photos are minutes apart: a burst's span is made up (about a third of a second a frame).
      suggestions: list.slice(0, limit).map(({ s, members }) => ({ id: s.id, kind: s.kind, evidence: s.evidence, primary: keeper(members).best.id, span_ms: 300 * members.length, items: members.map(out) })),
    }), true;
  }

  if (rest === "stacks/suggestions/dismiss" && method === "POST") {
    const body = await readJson(req);
    const ids = idList(body.ids, 100);
    if (!ids?.length) return fail(res, 400, "bad_request", "ids: at least one id"), true;
    state.dismissedSets.add([...ids].sort().join("|"));
    return send(res, 200, { ok: true }), true;
  }

  if (rest === "stacks" && method === "POST") {
    const body = await readJson(req);
    const ids = idList(body.ids, 100);
    if (!ids || ids.length < 2 || new Set(ids).size !== ids.length) return fail(res, 400, "bad_request", "ids: 2 to 100 distinct ids"), true;
    const primary = typeof body.primary === "string" ? body.primary : ids[0];
    if (!ids.includes(primary)) return fail(res, 400, "bad_request", "primary must be one of ids"), true;
    const kind = ["manual", "burst", "raw_jpeg"].includes(body.kind) ? body.kind : "manual";
    const failed = [];
    for (const id of ids) {
      const it = byId.get(id);
      if (!it) failed.push({ id, reason: "missing" });
      else if (it.trashed_at) failed.push({ id, reason: "trashed" });
      else if (it.kind !== "photo") failed.push({ id, reason: "not_photo" });
      else if (it.stack_id) failed.push({ id, reason: "already_stacked" });
      else if (it.hidden) failed.push({ id, reason: "hidden" });
    }
    if (failed.length) return send(res, 409, { ok: false, error: `${failed[0].id}: ${failed[0].reason}`, failed }), true;
    return send(res, 201, { ok: true, stack: stackOut(makeStack(items, ids, primary, kind), items, out) }), true;
  }

  if ((m = /^stacks\/([^/]+)$/.exec(rest))) {
    const stack = state.stacks.get(decodeURIComponent(m[1]));
    if (!stack) return fail(res, 404, "not_found", "no such stack"), true;
    if (method === "GET") return send(res, 200, { ok: true, stack: stackOut(stack, items, out) }), true;
    if (method === "PUT") {
      const body = await readJson(req);
      if (typeof body.primary !== "string" || !stack.ids.includes(body.primary)) return fail(res, 400, "bad_request", "primary must be a member"), true;
      byId.get(stack.primary).hidden = true;
      stack.primary = body.primary;
      byId.get(stack.primary).hidden = false;
      return send(res, 200, { ok: true, stack: stackOut(stack, items, out) }), true;
    }
    if (method === "DELETE") {
      for (const id of stack.ids) {
        const it = byId.get(id);
        it.hidden = false;
        it.stack_id = null;
      }
      state.stacks.delete(stack.id);
      return send(res, 200, { ok: true, unstacked: stack.ids }), true;
    }
  }
  return fail(res, 405, "method_not_allowed", "method not allowed"), true;
}
