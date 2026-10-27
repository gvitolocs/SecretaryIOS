// A synthetic Gallery for the dev mock: every `/v1/media` endpoint the web Gallery uses (docs/media-library.md, API),
// over generated items (coloured SVG pictures, invented device names and file names; never a real photo).
// Used when MOCK_MEDIA_URL is not set; with it set, mock/media.mjs proxies a real library instead.
//
//   MOCK_GALLERY_ITEMS=20000 npm run dev:mock     # how many items (default 6000)
//
// Items are newest first. Devices: a phone and a Mac (newer items), an old backup (older ones), plus chat shares and
// Lucina's own pictures. About 1 % of the items start in the trash. Edits and batch operations change the
// in-memory state, so the Gallery can be tried end to end; a restart resets it.
//
// Endpoints (all under /v1/media): GET items (origin, kind, from, to, q, favorite, trashed, album, cursor, limit),
// GET items/months, GET sources, GET/POST albums, GET items/{id}, GET items/{id}/details, PATCH items/{id},
// DELETE items/{id} (trash), POST items/{id}/restore, POST items/batch, GET items/{id}/(thumb|large|r/grid|
// r/preview|original) (SVG), GET devices/{id}/deletions, POST devices/{id}/deletions/ack.

const COUNT = Math.max(0, Number(process.env.MOCK_GALLERY_ITEMS ?? 6000) || 0);
const TRASH_DAYS = 30;
const DAY = 86_400_000;
const BATCH_MAX = 500;

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

// ---- ThumbHash (the reference encoder, https://evanw.github.io/thumbhash/) over a tiny synthetic gradient ----------

function thumbHash(w, h, rgba) {
  const max = Math.max(w, h);
  let avgR = 0, avgG = 0, avgB = 0;
  for (let i = 0, j = 0; i < w * h; i++, j += 4) {
    avgR += rgba[j]; avgG += rgba[j + 1]; avgB += rgba[j + 2];
  }
  avgR /= w * h * 255; avgG /= w * h * 255; avgB /= w * h * 255;
  const lx = Math.max(1, Math.round((7 * w) / max));
  const ly = Math.max(1, Math.round((7 * h) / max));
  const L = [], P = [], Q = [];
  for (let i = 0, j = 0; i < w * h; i++, j += 4) {
    const r = rgba[j] / 255, g = rgba[j + 1] / 255, b = rgba[j + 2] / 255;
    L[i] = (r + g + b) / 3;
    P[i] = (r + g) / 2 - b;
    Q[i] = r - g;
  }
  const encode = (channel, nx, ny) => {
    let dc = 0, scale = 0;
    const ac = [];
    for (let cy = 0; cy < ny; cy++) {
      for (let cx = 0; cx * ny < nx * (ny - cy); cx++) {
        let f = 0;
        for (let y = 0; y < h; y++) {
          const fy = Math.cos((Math.PI / h) * cy * (y + 0.5));
          for (let x = 0; x < w; x++) f += channel[x + y * w] * Math.cos((Math.PI / w) * cx * (x + 0.5)) * fy;
        }
        f /= w * h;
        if (cx || cy) { ac.push(f); scale = Math.max(scale, Math.abs(f)); } else dc = f;
      }
    }
    if (scale) for (let i = 0; i < ac.length; i++) ac[i] = 0.5 + (0.5 / scale) * ac[i];
    return [dc, ac, scale];
  };
  const [lDc, lAc, lScale] = encode(L, Math.max(3, lx), Math.max(3, ly));
  const [pDc, pAc, pScale] = encode(P, 3, 3);
  const [qDc, qAc, qScale] = encode(Q, 3, 3);
  const landscape = w > h;
  const h24 = Math.round(63 * lDc) | (Math.round(31.5 + 31.5 * pDc) << 6) | (Math.round(31.5 + 31.5 * qDc) << 12) | (Math.round(31 * lScale) << 18);
  const h16 = (landscape ? ly : lx) | (Math.round(63 * pScale) << 3) | (Math.round(63 * qScale) << 9) | (landscape << 15);
  const hash = [h24 & 255, (h24 >> 8) & 255, h24 >> 16, h16 & 255, h16 >> 8];
  let idx = 0;
  for (const ac of [lAc, pAc, qAc]) for (const f of ac) hash[5 + (idx >> 1)] = (hash[5 + (idx >> 1)] ?? 0) | (Math.round(15 * f) << ((idx++ & 1) << 2));
  return Buffer.from(hash).toString("base64");
}

function hsl(h, s, l) {
  const k = (n) => (n + h / 30) % 12;
  const a = s * Math.min(l, 1 - l);
  const f = (n) => l - a * Math.max(-1, Math.min(k(n) - 3, Math.min(9 - k(n), 1)));
  return [Math.round(255 * f(0)), Math.round(255 * f(8)), Math.round(255 * f(4))];
}

const hashCache = new Map();
function hashFor(hue, aspect) {
  const bucket = aspect >= 1.2 ? "l" : aspect <= 0.85 ? "p" : "s";
  const key = `${hue}|${bucket}`;
  let v = hashCache.get(key);
  if (!v) {
    const [w, h] = bucket === "l" ? [8, 6] : bucket === "p" ? [6, 8] : [6, 6];
    const rgba = new Uint8Array(w * h * 4);
    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        const [r, g, b] = hsl((hue + x * 6) % 360, 0.55, 0.28 + 0.4 * (y / h));
        rgba.set([r, g, b, 255], (y * w + x) * 4);
      }
    }
    v = thumbHash(w, h, rgba);
    hashCache.set(key, v);
  }
  return v;
}

// ---- the library -------------------------------------------------------------------------------------------------

const DEVICES = [
  { id: "iphone-test", name: "Test iPhone", kind: "iphone" },
  { id: "mac-test", name: "Test Mac", kind: "mac" },
  { id: "backup-2019", name: "Old backup", kind: "backup" },
];
const PLACES = [
  { name: "Aarhus", admin1: "Midtjylland", country: "Denmark", lat: 56.1629, lon: 10.2039 },
  { name: "Copenhagen", admin1: "Hovedstaden", country: "Denmark", lat: 55.6761, lon: 12.5683 },
  { name: "Rome", admin1: "Lazio", country: "Italy", lat: 41.9028, lon: 12.4964 },
  { name: "Lisbon", admin1: "Lisboa", country: "Portugal", lat: 38.7223, lon: -9.1393 },
];
const SHAPES = [
  [4032, 3024], [4032, 3024], [4032, 3024], [3024, 4032], [3024, 4032], [1920, 1080], [3000, 3000], [4000, 1500], [2400, 3600],
];

const albums = [
  { id: "alb_holiday", title: "Holiday (test)", system_key: null },
  { id: "alb_family", title: "Family (test)", system_key: null },
];
const deletions = new Map();

function build() {
  const rand = rng(20261028);
  const now = Date.now();
  const items = [];
  let t = now - 3_600_000;
  let deviceIx = 0;
  let event = 0;
  for (let i = 0; i < COUNT; i++) {
    // Photos come in bursts (one event); between events the device and the day change.
    if (rand() < 0.1 || i === 0) {
      t -= (6 + rand() * 70) * 3_600_000;
      event++;
      deviceIx = rand() < 0.62 ? 0 : 1;
    } else {
      t -= (10 + rand() * 600) * 1000;
    }
    const old = i > COUNT * 0.55;
    const r = rand();
    let device = null;
    let source = "lucina-generated";
    if (r < 0.07) { source = "ios-chat"; }
    else if (r < 0.1) { source = "lucina-generated"; }
    else if (old && r < 0.7) { device = DEVICES[2]; source = "import"; }
    else { device = DEVICES[deviceIx]; source = device.kind === "iphone" ? "ios-photos" : "mac-upload"; }
    const video = rand() < 0.12;
    const shape = video ? (rand() < 0.8 ? [1920, 1080] : [1080, 1920]) : SHAPES[Math.floor(rand() * SHAPES.length)];
    const takenAt = source === "ios-chat" && rand() < 0.5 ? null : new Date(t).toISOString();
    const addedAt = new Date(takenAt ? t + 3_600_000 * rand() * 4 : t).toISOString();
    const n = 1000 + Math.floor(rand() * 8999);
    const name = device?.kind === "iphone" ? `IMG_${n}.${video ? "MOV" : "HEIC"}`
      : device?.kind === "mac" ? `Screenshot ${n}.png`
      : device ? `DSC${String(n).padStart(5, "0")}.${video ? "AVI" : "JPG"}`
      : source === "ios-chat" ? `photo_${n}.jpg` : `lucina_${n}.png`;
    const place = rand() < 0.3 ? PLACES[Math.floor(rand() * PLACES.length)] : null;
    items.push({
      id: `itm_${i.toString(16).padStart(6, "0")}`,
      index: i,
      kind: video ? "video" : "photo",
      title: name,
      caption: rand() < 0.04 ? `Caption ${n}` : null,
      mime: video ? "video/mp4" : "image/jpeg",
      size: Math.round(shape[0] * shape[1] * (video ? 0.9 : 0.35)),
      taken_at: takenAt,
      created_at: addedAt,
      sort_at: takenAt ?? addedAt,
      width: shape[0],
      height: shape[1],
      duration_ms: video ? Math.round((3 + rand() * rand() * 400) * 1000) : null,
      place,
      camera: device?.kind === "iphone" ? "iPhone 15 Pro" : device?.kind === "backup" ? "Canon EOS 600D" : null,
      source,
      device: device?.id ?? null,
      device_name: device?.name ?? null,
      origin: {
        filename: name,
        folder: device?.kind === "backup" ? `DCIM/${new Date(t).getFullYear()}/100CANON` : device?.kind === "mac" ? "Pictures/Screenshots" : null,
        imported_from: device?.kind === "backup" ? "Old backup disk" : null,
      },
      favorite: rand() < 0.05,
      rotation: 0,
      albums: [],
      trashed_at: null,
      hue: Math.floor(rand() * 360),
    });
    if (rand() < 0.04) items[items.length - 1].albums.push(albums[Math.floor(rand() * albums.length)].id);
  }
  // Newest first, ties by id, like the library's paging order.
  items.sort((a, b) => (a.sort_at === b.sort_at ? (a.id < b.id ? 1 : -1) : a.sort_at < b.sort_at ? 1 : -1));
  // A few trashed items.
  const trashRand = rng(7);
  for (const it of items) if (trashRand() < 0.012) it.trashed_at = new Date(now - Math.floor(trashRand() * (TRASH_DAYS - 1)) * DAY - 3_600_000).toISOString();
  return items;
}

let items = build();
const byId = () => new Map(items.map((i) => [i.id, i]));

function expires(it) {
  return it.trashed_at ? new Date(Date.parse(it.trashed_at) + TRASH_DAYS * DAY).toISOString() : null;
}

function out(it) {
  const aspect = it.height ? it.width / it.height : 1;
  const base = `/v1/media/items/${it.id}`;
  return {
    id: it.id,
    kind: it.kind,
    title: it.title,
    caption: it.caption,
    mime: it.mime,
    size: it.size,
    taken_at: it.taken_at,
    created_at: it.created_at,
    width: it.width,
    height: it.height,
    duration_ms: it.duration_ms,
    place: it.place,
    camera: it.camera,
    source: it.source,
    device: it.device,
    device_name: it.device_name,
    origin: it.origin,
    favorite: it.favorite,
    rotation: it.rotation,
    albums: it.albums,
    trashed_at: it.trashed_at,
    trash_expires_at: expires(it),
    thumbhash: hashFor(it.hue, aspect),
    urls: {
      thumb: `${base}/thumb?size=256`,
      large: `${base}/thumb?size=1024`,
      original: `${base}/original`,
      download: `${base}/original?download=1`,
      ...(it.kind === "video" ? { poster: `${base}/thumb?size=1024` } : {}),
    },
  };
}

// ---- filters -----------------------------------------------------------------------------------------------------

function originMatches(it, origin) {
  if (!origin || origin === "all") return true;
  if (origin === "chat") return it.source.startsWith("ios-chat") || it.source.startsWith("ios-share") || it.source.startsWith("web-");
  if (origin === "lucina") return it.source.startsWith("lucina");
  if (origin.startsWith("device:")) return it.device === origin.slice(7);
  return false;
}

function matcher(p) {
  const kinds = new Set((p.get("kind") ?? "photo,video").split(",").filter(Boolean));
  const origin = p.get("origin");
  const trashed = p.get("trashed") === "true";
  const favorite = p.get("favorite") === "true";
  const q = (p.get("q") ?? "").trim().toLowerCase();
  const album = p.get("album");
  const from = p.get("from") ? Date.parse(`${p.get("from").slice(0, 10)}T00:00:00Z`) : null;
  const to = p.get("to") ? Date.parse(`${p.get("to").slice(0, 10)}T00:00:00Z`) + DAY : null;
  return (it) => {
    if (!kinds.has(it.kind)) return false;
    if (trashed !== (it.trashed_at !== null)) return false;
    if (!originMatches(it, origin)) return false;
    if (favorite && !it.favorite) return false;
    if (album && !it.albums.includes(album)) return false;
    if (from !== null || to !== null) {
      const ms = Date.parse(it.sort_at);
      if (from !== null && ms < from) return false;
      if (to !== null && ms >= to) return false;
    }
    if (q) {
      const hay = `${it.title} ${it.caption ?? ""} ${it.place?.name ?? ""} ${it.device_name ?? ""} ${it.origin.folder ?? ""}`.toLowerCase();
      if (!hay.includes(q)) return false;
    }
    return true;
  };
}

/** The index of the first item strictly after the cursor `<sort_at>|<id>` in the sorted list. */
function afterCursor(cursor) {
  if (!cursor) return 0;
  let decoded;
  try { decoded = Buffer.from(cursor, "base64url").toString("utf8"); } catch { return null; }
  const bar = decoded.lastIndexOf("|");
  if (bar < 0) return null;
  const at = decoded.slice(0, bar);
  const id = decoded.slice(bar + 1);
  let lo = 0, hi = items.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    const it = items[mid];
    const after = it.sort_at < at || (it.sort_at === at && it.id < id);
    if (after) hi = mid; else lo = mid + 1;
  }
  return lo;
}

const encodeCursor = (it) => Buffer.from(`${it.sort_at}|${it.id}`).toString("base64url");

// ---- images ------------------------------------------------------------------------------------------------------

function svgFor(it, size) {
  const aspect = it.height ? it.width / it.height : 1;
  const w = aspect >= 1 ? size : Math.round(size * aspect);
  const h = aspect >= 1 ? Math.round(size / aspect) : size;
  const a = `hsl(${it.hue} 55% 38%)`;
  const b = `hsl(${(it.hue + 40) % 360} 60% 62%)`;
  const day = it.sort_at.slice(0, 10);
  const fs = Math.max(10, Math.round(Math.min(w, h) / 9));
  const play = it.kind === "video"
    ? `<circle cx="${w / 2}" cy="${h / 2}" r="${Math.min(w, h) / 7}" fill="rgba(0,0,0,.45)"/><path d="M${w / 2 - fs * 0.6} ${h / 2 - fs} L${w / 2 + fs * 1.1} ${h / 2} L${w / 2 - fs * 0.6} ${h / 2 + fs} Z" fill="#fff"/>`
    : "";
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${a}"/><stop offset="1" stop-color="${b}"/></linearGradient></defs><rect width="${w}" height="${h}" fill="url(#g)"/><path d="M0 ${h} L${w * 0.38} ${h * 0.55} L${w * 0.6} ${h * 0.8} L${w * 0.78} ${h * 0.62} L${w} ${h} Z" fill="rgba(0,0,0,.22)"/><circle cx="${w * 0.78}" cy="${h * 0.22}" r="${Math.min(w, h) * 0.09}" fill="rgba(255,255,255,.55)"/><text x="${w * 0.06}" y="${h * 0.12 + fs}" font-family="monospace" font-size="${fs}" fill="#fff" fill-opacity=".9">#${it.index}</text><text x="${w * 0.06}" y="${h * 0.12 + fs * 2.2}" font-family="monospace" font-size="${Math.round(fs * 0.7)}" fill="#fff" fill-opacity=".75">${day}</text>${play}</svg>`;
}

// ---- handlers ----------------------------------------------------------------------------------------------------

function exifFor(it) {
  if (it.kind === "video" || !it.camera) return null;
  const phone = it.device && it.device.startsWith("iphone");
  return {
    make: phone ? "Apple" : "Canon",
    model: it.camera,
    lens: phone ? "iPhone 15 Pro back camera 6.86mm f/1.78" : "EF-S 18-55mm",
    iso: 80 + (it.index % 8) * 100,
    exposure_s: 1 / (60 + (it.index % 10) * 40),
    f_number: phone ? 1.78 : 4,
    focal_mm: phone ? 6.86 : 35,
    flash: it.index % 9 === 0,
    orientation: 1,
    software: phone ? "17.5" : null,
    taken_at: it.taken_at,
    offset: "+02:00",
  };
}

function trashIt(it, alsoFromDevice) {
  it.trashed_at = new Date().toISOString();
  if (alsoFromDevice && it.device) {
    const list = deletions.get(it.device) ?? [];
    list.push({ asset_id: `asset-${it.id}`, item_id: it.id, requested_at: it.trashed_at });
    deletions.set(it.device, list);
  }
}

function applyBatch(op, ids, body) {
  const map = byId();
  const done = [];
  const failed = [];
  const purged = new Set();
  for (const id of ids) {
    const it = map.get(id);
    if (!it) { failed.push({ id, error: "not_found" }); continue; }
    switch (op) {
      case "trash": trashIt(it, body.also_from_device === true); break;
      case "restore": it.trashed_at = null; break;
      case "purge":
        if (!it.trashed_at) { failed.push({ id, error: "not_trashed" }); continue; }
        purged.add(id);
        break;
      case "favorite": it.favorite = true; break;
      case "unfavorite": it.favorite = false; break;
      case "album_add": if (!it.albums.includes(body.album)) it.albums.push(body.album); break;
      case "album_remove": it.albums = it.albums.filter((a) => a !== body.album); break;
    }
    done.push(id);
  }
  if (purged.size) items = items.filter((i) => !purged.has(i.id));
  return { done, failed };
}

function image(res, it, size) {
  res.writeHead(200, { "Content-Type": "image/svg+xml", "Cache-Control": "private, max-age=3600" });
  res.end(svgFor(it, size));
}

/** Returns true when the request was answered. */
export async function handleGallery(req, res, { path, method, url, send, fail, readJson }) {
  if (!path.startsWith("/v1/media/")) return false;
  const rest = path.slice("/v1/media/".length);
  const p = url.searchParams;

  if (rest === "items" && method === "GET") {
    const keep = matcher(p);
    const limit = Math.min(500, Math.max(1, Number(p.get("limit")) || 200));
    let i = afterCursor(p.get("cursor"));
    if (i === null) return fail(res, 400, "bad_cursor", "invalid cursor"), true;
    const page = [];
    for (; i < items.length && page.length < limit; i++) if (keep(items[i])) page.push(items[i]);
    let next = null;
    if (page.length === limit) {
      for (let j = i; j < items.length; j++) if (keep(items[j])) { next = encodeCursor(page[page.length - 1]); break; }
    }
    send(res, 200, { items: page.map(out), next_cursor: next, total: null });
    return true;
  }

  if (rest === "items/months" && method === "GET") {
    const keep = matcher(p);
    const counts = new Map();
    for (const it of items) if (keep(it)) counts.set(it.sort_at.slice(0, 7), (counts.get(it.sort_at.slice(0, 7)) ?? 0) + 1);
    send(res, 200, { months: [...counts].sort((a, b) => (a[0] < b[0] ? 1 : -1)).map(([month, count]) => ({ month, count })) });
    return true;
  }

  if (rest === "sources" && method === "GET") {
    const groups = new Map();
    const add = (id, label, kind, it) => {
      const g = groups.get(id) ?? { id, label, kind, photos: 0, videos: 0, latest_taken_at: null };
      if (it.kind === "video") g.videos++; else g.photos++;
      if (!g.latest_taken_at || it.sort_at > g.latest_taken_at) g.latest_taken_at = it.sort_at;
      groups.set(id, g);
    };
    const all = { photos: 0, videos: 0 };
    for (const it of items) {
      if (it.trashed_at) continue;
      if (it.kind === "video") all.videos++; else all.photos++;
      if (it.device) {
        const d = DEVICES.find((x) => x.id === it.device);
        add(`device:${it.device}`, d.name, d.kind, it);
      } else if (originMatches(it, "chat")) add("chat", "Chat & shares", "chat", it);
      else add("lucina", "Lucina", "lucina", it);
    }
    send(res, 200, { sources: [...groups.values()].sort((a, b) => (a.latest_taken_at < b.latest_taken_at ? 1 : -1)), all });
    return true;
  }

  if (rest === "albums") {
    if (method === "GET") {
      send(res, 200, { albums: albums.map((a) => ({ ...a, count: items.filter((i) => !i.trashed_at && i.albums.includes(a.id)).length })) });
      return true;
    }
    if (method === "POST") {
      const body = await readJson(req);
      const title = typeof body.title === "string" ? body.title.trim() : "";
      if (!title) return fail(res, 400, "bad_request", "title required"), true;
      const album = { id: `alb_${albums.length + 1}_${Date.now().toString(36)}`, title, system_key: null };
      albums.push(album);
      send(res, 201, { ...album, count: 0 });
      return true;
    }
  }

  if (rest === "items/batch" && method === "POST") {
    const body = await readJson(req);
    const ops = ["trash", "restore", "purge", "favorite", "unfavorite", "album_add", "album_remove"];
    if (!ops.includes(body.op)) return fail(res, 400, "bad_op", "unknown op"), true;
    if (!Array.isArray(body.ids) || body.ids.length === 0 || body.ids.length > BATCH_MAX || body.ids.some((x) => typeof x !== "string")) {
      return fail(res, 400, "bad_ids", `ids: 1 to ${BATCH_MAX} item ids`), true;
    }
    if ((body.op === "album_add" || body.op === "album_remove") && !albums.some((a) => a.id === body.album)) return fail(res, 404, "no_album", "unknown album"), true;
    send(res, 200, applyBatch(body.op, body.ids, body));
    return true;
  }

  const dev = rest.match(/^devices\/([^/]+)\/deletions(\/ack)?$/);
  if (dev) {
    const id = decodeURIComponent(dev[1]);
    if (!dev[2] && method === "GET") {
      send(res, 200, { pending: deletions.get(id) ?? [] });
      return true;
    }
    if (dev[2] && method === "POST") {
      const body = await readJson(req);
      const acked = new Set(Array.isArray(body.asset_ids) ? body.asset_ids : []);
      deletions.set(id, (deletions.get(id) ?? []).filter((d) => !acked.has(d.asset_id)));
      send(res, 200, { ok: true });
      return true;
    }
  }

  const one = rest.match(/^items\/([^/]+)(?:\/(.*))?$/);
  if (one) {
    const it = items.find((i) => i.id === decodeURIComponent(one[1]));
    const sub = one[2] ?? "";
    if (!it) return fail(res, 404, "not_found", "no such item"), true;
    if (sub === "" && method === "GET") return send(res, 200, out(it)), true;
    if (sub === "details" && method === "GET") return send(res, 200, { item: out(it), exif: exifFor(it), people: [] }), true;
    if (sub === "" && method === "PATCH") {
      const body = await readJson(req);
      if (body.rotation !== undefined && ![0, 90, 180, 270].includes(body.rotation)) return fail(res, 400, "bad_rotation", "rotation: 0, 90, 180 or 270"), true;
      if (typeof body.title === "string") it.title = body.title;
      if (typeof body.caption === "string") it.caption = body.caption || null;
      if (typeof body.favorite === "boolean") it.favorite = body.favorite;
      if (body.rotation !== undefined) it.rotation = body.rotation;
      if (typeof body.taken_at === "string") { it.taken_at = body.taken_at; it.sort_at = body.taken_at; }
      return send(res, 200, out(it)), true;
    }
    if (sub === "" && method === "DELETE") return trashIt(it, false), send(res, 200, { ok: true }), true;
    if (sub === "restore" && method === "POST") return (it.trashed_at = null), send(res, 200, out(it)), true;
    if (method === "GET" && (sub === "thumb" || sub === "r/grid" || sub === "r/preview" || sub === "original")) {
      const size = sub === "original" ? 1600 : Math.min(2000, Math.max(32, Number(p.get("size")) || (sub === "r/preview" ? 1440 : 256)));
      return image(res, it, size), true;
    }
  }
  return false;
}
