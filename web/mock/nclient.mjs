// NClient for the mock server (docs/nclient.md, "HTTP"): /v1/nclient/* over synthetic galleries.
//
// Everything here is neutral placeholder content: titles are "Colour Study 01", tags are shapes and
// colours, and every image is a generated SVG (solid colours and abstract shapes with a page number).
// No real gallery, title, tag or picture is involved. Favourites, reading progress and "Save to
// Lucina" jobs live in memory.
//
//   POST /v1/_mock/nclient {"enabled": false}  -> /status answers enabled:false (the plugin shows "unavailable")
//   POST /v1/_mock/nclient {"media": "down"}   -> a save job fails with media_unavailable after 3 pages
//   POST /v1/_mock/nclient {"slow": true}      -> save jobs take 1.5 s per page instead of 0.35 s

import { createHmac, randomUUID } from "node:crypto";

const KEY = "mock-nclient-signing-key";
const PAGE_SIZE = 12;

const TAGS = [
  "colour", "geometry", "stripes", "dots", "gradient", "monochrome", "circles", "triangles", "waves", "grid", "sketch", "paper",
].map((name, i) => ({ id: 100 + i, name, type: "tag", count: 900 - i * 60 }));

const WORDS = ["Colour", "Shape", "Pattern", "Light", "Texture", "Line", "Study", "Sample", "Layout", "Form"];
const PALETTES = [
  ["#1e3a5f", "#f2c14e", "#f78154"], ["#2d6a4f", "#d8f3dc", "#95d5b2"], ["#5b2a86", "#e0aaff", "#9d4edd"],
  ["#7f1d1d", "#fecaca", "#f97316"], ["#0f4c5c", "#e36414", "#fb8b24"], ["#264653", "#e9c46a", "#e76f51"],
  ["#3d405b", "#f4f1de", "#e07a5f"], ["#14213d", "#fca311", "#e5e5e5"],
];

const galleries = Array.from({ length: 30 }, (_, i) => {
  const id = 1000 + i * 7;
  const pages = 8 + ((i * 5) % 17); // 8..24
  return {
    id,
    title: `${WORDS[i % WORDS.length]} ${WORDS[(i * 3 + 1) % WORDS.length]} ${String(i + 1).padStart(2, "0")}`,
    pages,
    language: i % 4 === 0 ? "japanese" : "english",
    tagIds: [100 + (i % 12), 100 + ((i * 5 + 3) % 12), 100 + ((i * 7 + 6) % 12)].filter((v, k, a) => a.indexOf(v) === k),
    palette: PALETTES[i % PALETTES.length],
    uploaded: 1_760_000_000 + i * 86_400,
    favorites: 20 + i * 3,
  };
});

const favorites = new Map(); // id -> addedAt
const progress = new Map(); // id -> {page, pages, updated_at}
const jobs = new Map();
const flags = { enabled: true, mediaDown: false, slow: false };

// Pre-seeded so the screenshots show a favourite and a half-read gallery.
favorites.set(galleries[2].id, Math.floor(Date.now() / 1000) - 86_400);
progress.set(galleries[1].id, { page: 5, pages: galleries[1].pages, updated_at: Math.floor(Date.now() / 1000) - 3600 });

const sign = (msg) => createHmac("sha256", KEY).update(msg).digest("hex").slice(0, 32);
function url(id, kind, index = 0) {
  const exp = Math.floor(Date.now() / 1000) + 6 * 3600;
  const msg = `${id}/${kind}/${index}/${exp}`;
  return `/v1/nclient/s/${msg}/${sign(msg)}`;
}
const tagOf = (id) => TAGS.find((t) => t.id === id);

function summary(g) {
  return {
    id: g.id, title: g.title, pages: g.pages, language: g.language,
    tags: g.tagIds.map(tagOf), thumbUrl: url(g.id, "thumb"),
    favorite: favorites.has(g.id), progress: progress.get(g.id) ?? null,
  };
}

function detail(g) {
  const related = galleries.filter((o) => o.id !== g.id && o.tagIds.some((t) => g.tagIds.includes(t))).slice(0, 6);
  return {
    id: g.id, title: g.title, titles: { english: g.title, japanese: "", pretty: g.title },
    tags: g.tagIds.map(tagOf), language: g.language, uploaded: g.uploaded, favorites: g.favorites,
    coverUrl: url(g.id, "cover"), thumbUrl: url(g.id, "thumb"),
    pages: Array.from({ length: g.pages }, (_, index) => ({
      index, width: 800, height: 1200, url: url(g.id, "page", index), thumbUrl: url(g.id, "pthumb", index),
    })),
    related: related.map(summary), favorite: favorites.has(g.id), progress: progress.get(g.id) ?? null,
  };
}

/** A neutral picture: solid colour, a few abstract shapes, and the page number. */
function svg(g, kind, index) {
  const [bg, a, b] = g.palette;
  const [w, h] = kind === "page" ? [800, 1200] : [400, 600];
  const n = index + 1;
  const shift = (index * 37) % 200;
  const label = kind === "page" || kind === "pthumb" ? String(n) : "";
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">
<rect width="${w}" height="${h}" fill="${bg}"/>
<circle cx="${w * 0.3 + shift / 4}" cy="${h * 0.3}" r="${w * 0.22}" fill="${a}" opacity=".9"/>
<rect x="${w * 0.45}" y="${h * 0.45 + shift / 3}" width="${w * 0.42}" height="${w * 0.42}" rx="${w * 0.04}" fill="${b}" opacity=".85"/>
<polygon points="${w * 0.1},${h * 0.9} ${w * 0.5},${h * 0.62} ${w * 0.62},${h * 0.92}" fill="${a}" opacity=".7"/>
<path d="M0 ${h * 0.75} Q ${w * 0.25} ${h * 0.68} ${w * 0.5} ${h * 0.75} T ${w} ${h * 0.75}" stroke="${b}" stroke-width="${w * 0.015}" fill="none"/>
${label ? `<text x="${w / 2}" y="${h * 0.56}" text-anchor="middle" font-family="sans-serif" font-size="${kind === "page" ? 220 : 120}" font-weight="700" fill="#fff" fill-opacity=".92">${label}</text>` : ""}
</svg>`;
}

/** GET /v1/nclient/s/<id>/<kind>/<index>/<exp>/<sig>: no session needed, like the signed URLs on p1. */
export function serveImage(req, res, path) {
  const m = /^\/v1\/nclient\/s\/(\d+)\/(thumb|cover|page|pthumb)\/(\d+)\/(\d+)\/([0-9a-f]+)$/.exec(path);
  const fail = (code) => { res.writeHead(code, { "Cache-Control": "no-store" }); res.end(); };
  if (!m) return fail(404);
  const [, id, kind, index, exp, sig] = m;
  if (sig !== sign(`${id}/${kind}/${index}/${exp}`) || Number(exp) < Date.now() / 1000) return fail(403);
  const g = galleries.find((x) => x.id === Number(id));
  if (!g || (kind.startsWith("p") && kind !== "pthumb" && Number(index) >= g.pages)) return fail(404);
  res.writeHead(200, { "Content-Type": "image/svg+xml", "Cache-Control": "private, max-age=3600", "Referrer-Policy": "no-referrer" });
  res.end(svg(g, kind, Number(index)));
}

function jobView(job) {
  const step = flags.slow ? 1500 : 350;
  let done = Math.min(job.total, Math.floor((Date.now() - job.started) / step));
  let state = job.state;
  if (state === "cancelled") done = job.doneAtCancel;
  else if (flags.mediaDown && done >= 3) { done = 3; state = "failed"; }
  else state = done >= job.total ? "done" : done > 0 || Date.now() > job.started + 150 ? "running" : "queued";
  const saved = state === "failed" ? [] : Array.from({ length: done }, (_, index) => ({ index, itemId: `item-${job.id.slice(0, 6)}-${index}`, duplicate: index === 1 && job.again }));
  const failed = state === "failed" ? Array.from({ length: done }, (_, index) => ({ index, error: "media_unavailable" })) : [];
  const out = { job: job.id, state, total: job.total, done, saved, failed };
  if (state === "failed") out.error = "media_unavailable";
  return out;
}

export async function handleNClient(req, res, { path, method, url: u, send, fail, readJson }) {
  if (path === "/v1/_mock/nclient" && method === "POST") {
    const body = await readJson(req);
    if (typeof body.enabled === "boolean") flags.enabled = body.enabled;
    if (body.media !== undefined) flags.mediaDown = body.media === "down";
    if (typeof body.slow === "boolean") flags.slow = body.slow;
    return send(res, 200, flags), true;
  }
  if (!path.startsWith("/v1/nclient")) return false;
  const rest = path.slice("/v1/nclient".length) || "/";
  const ok = (data, status = 200) => send(res, status, { ok: true, data });
  const q = u.searchParams;

  if (rest === "/status" && method === "GET") return ok({ enabled: flags.enabled, rateLimitPerSecond: 1, save: { gallery: true, library: false } }), true;
  if (!flags.enabled) return fail(res, 503, "unavailable", "NClient is off."), true;

  if (rest === "/search" && method === "GET") {
    const text = (q.get("q") ?? "").trim().toLowerCase();
    const inc = (q.get("tags") ?? "").split(",").filter(Boolean).map(Number);
    const exc = (q.get("exclude") ?? "").split(",").filter(Boolean).map(Number);
    let list = galleries.filter((g) => (!text || g.title.toLowerCase().includes(text)) && inc.every((t) => g.tagIds.includes(t)) && !exc.some((t) => g.tagIds.includes(t)));
    const sort = q.get("sort") ?? "recent";
    list = [...list].sort((a, b) => (sort === "recent" ? b.uploaded - a.uploaded : b.favorites - a.favorites));
    const page = Math.max(1, Number(q.get("page")) || 1);
    return ok({ items: list.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE).map(summary), page, numPages: Math.max(1, Math.ceil(list.length / PAGE_SIZE)) }), true;
  }
  if (rest === "/random" && method === "GET") return ok(detail(galleries[Math.floor(Math.random() * galleries.length)])), true;
  if (rest === "/tags" && method === "GET") {
    const text = (q.get("q") ?? "").toLowerCase();
    return ok({ tags: TAGS.filter((t) => t.name.includes(text)).slice(0, Number(q.get("limit")) || 50) }), true;
  }
  if (rest === "/favorites" && method === "GET") {
    return ok({ items: [...favorites].map(([id, addedAt]) => ({ ...summary(galleries.find((g) => g.id === id)), addedAt })) }), true;
  }
  let m = /^\/favorites\/(\d+)$/.exec(rest);
  if (m) {
    const g = galleries.find((x) => x.id === Number(m[1]));
    if (!g) return fail(res, 404, "not_found", "Not found."), true;
    if (method === "PUT") favorites.set(g.id, Math.floor(Date.now() / 1000));
    else if (method === "DELETE") favorites.delete(g.id);
    else return false;
    return ok({ favorite: favorites.has(g.id) }), true;
  }
  if (rest === "/progress" && method === "GET") return ok({ progress: Object.fromEntries(progress) }), true;
  m = /^\/progress\/(\d+)$/.exec(rest);
  if (m && method === "PUT") {
    const body = await readJson(req);
    progress.set(Number(m[1]), { page: Number(body.page) || 0, pages: Number(body.pages) || 0, updated_at: Math.floor(Date.now() / 1000) });
    return ok({ page: progress.get(Number(m[1])).page }), true;
  }
  m = /^\/galleries\/(\d+)(\/related|\/save)?$/.exec(rest);
  if (m) {
    const g = galleries.find((x) => x.id === Number(m[1]));
    if (!g) return fail(res, 404, "not_found", "Not found."), true;
    if (!m[2] && method === "GET") return ok(detail(g)), true;
    if (m[2] === "/related" && method === "GET") return ok({ items: detail(g).related }), true;
    if (m[2] === "/save" && method === "POST") {
      const body = await readJson(req).catch(() => ({}));
      if ((body.target ?? "gallery") === "library") return fail(res, 400, "library_not_supported", "The Library does not take comics."), true;
      const running = [...jobs.values()].find((j) => j.gallery === g.id && jobView(j).state !== "done" && jobView(j).state !== "cancelled" && jobView(j).state !== "failed");
      if (running) return ok(jobView(running), 202), true;
      const job = { id: randomUUID().replaceAll("-", "").slice(0, 24), gallery: g.id, total: g.pages, started: Date.now(), state: "running", again: jobs.size > 0 };
      jobs.set(job.id, job);
      return ok(jobView(job), 202), true;
    }
  }
  m = /^\/saves\/([0-9a-f]+)$/.exec(rest);
  if (m) {
    const job = jobs.get(m[1]);
    if (!job) return fail(res, 404, "not_found", "Not found."), true;
    if (method === "GET") return ok(jobView(job)), true;
    if (method === "DELETE") {
      const view = jobView(job);
      if (view.state === "running" || view.state === "queued") { job.state = "cancelled"; job.doneAtCancel = view.done; }
      return ok({ cancelling: true }), true;
    }
  }
  return false;
}
