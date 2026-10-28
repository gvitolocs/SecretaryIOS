// A synthetic Pixiv for the dev mock: every `/v1/pixiv` endpoint the web Pixiv plugin uses (docs/pixiv.md),
// over generated works. The pictures are generated SVG landscapes (never real artwork); about one work in
// nine is marked R-18 so the adult switch can be tried (its picture is just as plain).
//
//   MOCK_PIXIV_WORKS=300 npm run dev:mock     # how many works (default 160)
//
// Like the real service: R-18 modes/searches/works answer 403 adult_off unless `adult=1`, R-18 items never
// appear in lists without it, answers are {ok:true,data} / {ok:false,error}, and image URLs are signed-looking
// `/v1/pixiv/s/...` paths that need no session. Bookmarks and saves live in memory.

const COUNT = Math.max(1, Number(process.env.MOCK_PIXIV_WORKS ?? 160) || 160);
const PAGE = 30;

function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const WORDS = ["Morning", "Harbour", "Lantern", "Rain", "Summer", "Garden", "Night", "Station", "Bridge", "Fox", "Tea", "Snow", "Kite", "Window", "Orchard", "Moon"];
const NOUNS = ["Light", "Walk", "Letter", "Dream", "Festival", "Road", "Study", "Sketch", "Memory", "Hours"];
const ARTISTS = ["Aoi Hoshino", "Mika Tanabe", "Leif Madsen", "Rin Okada", "Sofie Brandt", "Kenji Mori"];
const TAGS = ["landscape", "original", "sky", "cat", "city", "風景", "sketch", "night", "flowers", "sea", "train", "tea"];

const works = [];
{
  const r = rng(77);
  for (let i = 0; i < COUNT; i++) {
    const id = String(10001 + i * 37);
    const pages = r() < 0.55 ? 1 : 2 + Math.floor(r() * 6);
    const shape = r();
    const [w, h] = shape < 0.4 ? [900, 1200] : shape < 0.7 ? [1200, 900] : shape < 0.85 ? [1000, 1000] : [800, 1300];
    const tags = [...new Set([TAGS[Math.floor(r() * TAGS.length)], TAGS[Math.floor(r() * TAGS.length)], TAGS[Math.floor(r() * TAGS.length)]])];
    works.push({
      id,
      index: i,
      title: `${WORDS[Math.floor(r() * WORDS.length)]} ${NOUNS[Math.floor(r() * NOUNS.length)]}`,
      artist: Math.floor(r() * ARTISTS.length),
      tags,
      pages,
      w,
      h,
      hue: Math.floor(r() * 360),
      r18: i % 9 === 4,
      kind: pages > 1 && r() < 0.6 ? "manga" : r() < 0.06 ? "ugoira" : "illust",
      views: Math.floor(r() * 90000),
      likes: Math.floor(r() * 5000),
      created: new Date(Date.UTC(2026, 9, 28) - i * 3_600_000 * 7).toISOString(),
    });
  }
}
const byId = new Map(works.map((w) => [w.id, w]));
const bookmarks = new Map();
const saved = [];

const RANKING = ["day", "week", "month", "day_male", "day_female", "original", "rookie", "day_r18", "week_r18", "day_male_r18", "day_female_r18"];

function sign(id, kind, n) {
  return `/v1/pixiv/s/${Math.floor(Date.now() / 1000) + 21600}/${id}.${kind}.${n}/${"0".repeat(32)}`;
}

function item(w) {
  return {
    id: w.id,
    title: w.title,
    author: ARTISTS[w.artist],
    authorId: String(1000 + w.artist),
    tags: w.tags,
    thumbUrl: sign(w.id, "t", 0),
    pages: w.pages,
    r18: w.r18,
    bookmarked: bookmarks.has(w.id),
  };
}

function listing(all, page, adult) {
  const shown = adult ? all : all.filter((w) => !w.r18);
  const slice = shown.slice((page - 1) * PAGE, page * PAGE);
  const numPages = Math.max(1, Math.ceil(shown.length / PAGE));
  return { items: slice.map(item), page, hasMore: page < numPages, total: shown.length, numPages };
}

function detail(w) {
  return {
    ...item(w),
    authorId: String(1000 + w.artist),
    created: w.created,
    kind: w.kind,
    ugoira: w.kind === "ugoira",
    description: `A generated placeholder work (${w.pages} page${w.pages > 1 ? "s" : ""}). Nothing here is real artwork.`,
    stats: { views: w.views, likes: w.likes, bookmarks: Math.floor(w.likes / 3), comments: Math.floor(w.likes / 40) },
    pageCount: w.pages,
    coverUrl: sign(w.id, "p", 0),
    tags: w.tags.map((tag) => ({ tag, en: tag === "風景" ? "landscape" : "" })),
    pages: Array.from({ length: w.pages }, (_, n) => ({ index: n, thumbUrl: sign(w.id, "t", n), url: sign(w.id, "p", n), originalUrl: sign(w.id, "o", n) })),
  };
}

function svgFor(w, kind, n) {
  const thumb = kind === "t";
  const [pw, ph] = thumb ? [w.w / 3, w.h / 3] : [w.w, w.h];
  const hue = (w.hue + n * 23) % 360;
  const c1 = `hsl(${hue} 60% 72%)`;
  const c2 = `hsl(${(hue + 40) % 360} 55% 45%)`;
  const c3 = `hsl(${(hue + 200) % 360} 40% 28%)`;
  const fs = Math.round(ph * 0.07);
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${pw}" height="${ph}" viewBox="0 0 ${pw} ${ph}"><defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${c1}"/><stop offset="1" stop-color="${c2}"/></linearGradient></defs><rect width="${pw}" height="${ph}" fill="url(#g)"/><circle cx="${pw * (0.25 + 0.5 * ((n * 0.37 + w.hue / 360) % 1))}" cy="${ph * 0.25}" r="${Math.min(pw, ph) * 0.1}" fill="#fff" fill-opacity=".7"/><path d="M0 ${ph} L0 ${ph * 0.66} Q${pw * 0.25} ${ph * 0.5} ${pw * 0.5} ${ph * 0.68} T${pw} ${ph * 0.6} L${pw} ${ph} Z" fill="${c3}" fill-opacity=".8"/><path d="M0 ${ph} L0 ${ph * 0.82} Q${pw * 0.3} ${ph * 0.7} ${pw * 0.6} ${ph * 0.84} T${pw} ${ph * 0.78} L${pw} ${ph} Z" fill="#000" fill-opacity=".25"/><text x="${pw * 0.06}" y="${ph * 0.1 + fs}" font-family="monospace" font-size="${fs}" fill="#fff" fill-opacity=".92">#${w.id} p${n + 1}/${w.pages}</text></svg>`;
}

export async function handlePixiv(req, res, { path, method, url, send, fail, readJson }) {
  if (!path.startsWith("/v1/pixiv/")) return false;
  const rest = path.slice("/v1/pixiv/".length);
  const p = url.searchParams;
  const adult = p.get("adult") === "1" || p.get("adult") === "true";
  const ok = (data) => send(res, 200, { ok: true, data });
  const err = (status, code, extra = {}) => send(res, status, { ok: false, error: code, ...extra });

  if (rest.startsWith("s/")) {
    const m = /^s\/\d+\/(\d+)\.([tpo])\.(\d+)\//.exec(rest);
    const w = m && byId.get(m[1]);
    if (!w) return fail(res, 403, "forbidden", "bad signature"), true;
    res.writeHead(200, { "Content-Type": "image/svg+xml", "Cache-Control": "private, max-age=3600" });
    res.end(svgFor(w, m[2], Number(m[3])));
    return true;
  }

  if (rest === "status" && method === "GET") return ok({ enabled: true, cacheBytes: 0, cacheMaxBytes: 2147483648, apiIntervalMs: 1500 }), true;

  if (rest === "ranking" && method === "GET") {
    const mode = p.get("mode") ?? "day";
    if (!RANKING.includes(mode)) return err(400, "bad_request"), true;
    if (mode.endsWith("_r18") && !adult) return err(403, "adult_off"), true;
    // A ranking is a fixed shuffle of the works per mode.
    const r = rng([...mode].reduce((a, c) => a + c.charCodeAt(0), 0));
    const pool = works.filter((w) => (mode.endsWith("_r18") ? w.r18 : true));
    const order = pool.map((w) => [r(), w]).sort((a, b) => a[0] - b[0]).map((x) => x[1]);
    return ok(listing(order, Math.max(1, Number(p.get("page")) || 1), adult)), true;
  }

  if (rest === "search" && method === "GET") {
    const q = (p.get("q") ?? "").trim().toLowerCase();
    if (!q) return err(400, "bad_request"), true;
    const mode = p.get("mode") ?? "safe";
    if (mode === "r18" && !adult) return err(403, "adult_off"), true;
    const type = p.get("type") ?? "all";
    let found = works.filter((w) => (w.title.toLowerCase().includes(q) || w.tags.some((t) => t.toLowerCase().includes(q)) || ARTISTS[w.artist].toLowerCase().includes(q)) && (type === "all" || w.kind === type));
    if (mode === "r18") found = found.filter((w) => w.r18);
    else if (mode === "safe") found = found.filter((w) => !w.r18);
    if (p.get("order") === "date") found = [...found].reverse();
    return ok(listing(found, Math.max(1, Number(p.get("page")) || 1), adult)), true;
  }

  const ill = /^illust\/(\d+)$/.exec(rest);
  if (ill && method === "GET") {
    const w = byId.get(ill[1]);
    if (!w) return err(404, "not_found"), true;
    if (w.r18 && !adult) return err(403, "adult_off"), true;
    return ok(detail(w)), true;
  }

  if (rest === "bookmarks" && method === "GET") {
    const items = [...bookmarks.entries()].reverse().map(([id]) => item(byId.get(id))).filter((i) => adult || !i.r18);
    return ok({ items }), true;
  }
  const bm = /^bookmarks\/(\d+)$/.exec(rest);
  if (bm && method === "PUT") {
    const w = byId.get(bm[1]);
    if (!w) return err(404, "not_found"), true;
    if (w.r18 && !adult) return err(403, "adult_off"), true;
    bookmarks.set(w.id, Date.now());
    return ok({ bookmarked: true }), true;
  }
  if (bm && method === "DELETE") {
    bookmarks.delete(bm[1]);
    return ok({ bookmarked: false }), true;
  }

  if (rest === "save" && method === "POST") {
    const body = await readJson(req);
    const w = byId.get(String(body.id));
    if (!w) return err(404, "not_found"), true;
    if (w.r18 && !(adult || body.adult === true)) return err(403, "adult_off"), true;
    const pages = Array.isArray(body.pages) && body.pages.length ? body.pages : Array.from({ length: w.pages }, (_, i) => i);
    if (pages.some((i) => !Number.isInteger(i) || i < 0 || i >= w.pages)) return err(400, "bad_request"), true;
    const out = pages.map((page) => {
      const itemId = `itm_px_${w.id}_${page}`;
      const duplicate = saved.includes(itemId);
      if (!duplicate) saved.push(itemId);
      return { page, itemId, duplicate };
    });
    return ok({ saved: out, failed: [] }), true;
  }

  return false;
}
