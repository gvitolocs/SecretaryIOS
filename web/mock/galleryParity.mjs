// Share links and one item's stored forms for the dev mock (docs/media-library.md: "Links over chosen photos",
// "One item's stored forms"). Runs before mock/gallery.mjs; the pictures themselves are gallery.mjs's SVGs.
// State is in memory: a restart resets it.

const DAY = 86_400_000;
const shares = [];
let counter = 0;

const token = () => `${(++counter).toString(36).padStart(4, "0")}${Math.random().toString(36).slice(2, 20)}`.padEnd(22, "A").slice(0, 22);

function out(s) {
  const expired = s.expires_at && Date.parse(s.expires_at) <= Date.now();
  return {
    token: s.token, url_path: `/v1/media/a/${s.token}`, public_path: `/a/${s.token}`, kind: s.kind, album_id: s.album_id,
    title: s.title, count: s.count, cover_item: s.cover_item, cover_thumb: s.cover_item ? `/v1/media/items/${s.cover_item}/thumb?size=256` : null,
    created_at: s.created_at, expires_at: s.expires_at, revoked_at: s.revoked_at, allow_original: s.allow_original,
    views: s.views, last_view_at: s.last_view_at, live: !s.revoked_at && !expired,
  };
}

function make(body, kind, albumId, title, ids) {
  const now = Date.now();
  const s = {
    token: token(), kind, album_id: albumId, title, count: ids.length, cover_item: ids[0] ?? null,
    created_at: new Date(now).toISOString(),
    expires_at: body.expires_in_days ? new Date(now + Math.min(3650, Math.max(1, body.expires_in_days)) * DAY).toISOString() : null,
    revoked_at: null, allow_original: body.allow_original === true, views: kind === "album" ? 12 : 0, last_view_at: null,
  };
  shares.unshift(s);
  return s;
}

/** One item's forms: a HEIC original from a phone with an older edited version and the phone's own JPEG. */
function renditions(id) {
  const n = [...id].reduce((a, c) => a + c.charCodeAt(0), 0);
  const base = `/v1/media/items/${id}`;
  const heic = n % 3 === 0;
  const hash = (seed) => (seed.toString(16).repeat(64)).slice(0, 64);
  const original = { version: 2, format: heic ? "heic" : "jpeg", mime: heic ? "image/heic" : "image/jpeg", bytes: 2_412_000, blake3: hash(10 + (n % 5)), width: 4032, height: 3024, stored: "store", reachable: true, url: `${base}/original` };
  const withDevice = n % 7 !== 0;
  return {
    item_id: id, kind: "photo", original,
    versions: [{ version: 1, format: original.format, mime: original.mime, bytes: 2_198_000, blake3: hash(3), replaced_at: "2026-10-20T10:00:00.000Z", stored: "store", reachable: true, url: `${base}/original?version=1` }],
    renditions: [
      { name: "grid", format: "jxl", mime: "image/jxl", bytes: 8_800, width: 256, height: 256, url: `${base}/r/grid?v=2` },
      { name: "preview", format: "jxl", mime: "image/jxl", bytes: 57_000, width: 1440, height: 1080, url: `${base}/r/preview?v=2` },
      { name: "thumb", format: "jpeg", mime: "image/jpeg", bytes: 38_700, width: 480, height: 360, url: `${base}/thumb?size=256&v=2` },
      { name: "large", format: "jpeg", mime: "image/jpeg", bytes: 135_000, width: 1600, height: 1200, url: `${base}/thumb?size=1024&v=2` },
      ...(heic ? [{ name: "device_preview", format: "jpeg", mime: "image/jpeg", bytes: 910_000, width: 4032, height: 3024, url: `${base}/thumb?size=2000&v=2` }] : []),
    ],
    device: withDevice ? {
      id: "ios-test", name: "Test iPhone", kind: "iphone", last_seen_at: "2026-10-28T08:00:00.000Z", asset_id: `ASSET-${n}`,
      uploads: [
        { filename: `IMG_${1000 + (n % 900)}.${heic ? "HEIC" : "JPG"}`, mime: original.mime, uti: heic ? "public.heic" : "public.jpeg", bytes: 2_198_000, blake3: hash(3), width: 4032, height: 3024, taken_at: null, uploaded_at: "2026-10-20T09:00:00.000Z", matches_version: 1 },
        { filename: `IMG_${1000 + (n % 900)}.${heic ? "HEIC" : "JPG"}`, mime: original.mime, uti: heic ? "public.heic" : "public.jpeg", bytes: 2_412_000, blake3: n % 2 ? hash(10 + (n % 5)) : null, width: 4032, height: 3024, taken_at: null, uploaded_at: "2026-10-27T09:00:00.000Z", matches_version: n % 2 ? 2 : null },
      ],
    } : null,
  };
}

/** Returns true when the request was answered. */
export async function handleGalleryParity(req, res, { path, method, send, fail, readJson }) {
  if (!path.startsWith("/v1/media/")) return false;
  const rest = path.slice("/v1/media/".length);
  let m;
  if (rest === "shares" && method === "GET") return send(res, 200, { shares: shares.map(out) }), true;
  if (rest === "shares" && method === "POST") {
    const body = await readJson(req);
    const ids = Array.isArray(body.item_ids) ? [...new Set(body.item_ids)] : [];
    if (!ids.length || ids.length > 500) return fail(res, 400, "bad_request", "item_ids must name 1 to 500 items"), true;
    return send(res, 201, { ...out(make(body, "selection", null, (body.title ?? "").trim() || "Shared photos", ids)), skipped: [] }), true;
  }
  if ((m = /^albums\/([^/]+)\/shares$/.exec(rest)) && method === "POST") {
    const body = await readJson(req);
    return send(res, 201, out(make(body, "album", decodeURIComponent(m[1]), "Album", ["a", "b", "c"]))), true;
  }
  if ((m = /^shares\/([^/]+)$/.exec(rest)) && method === "DELETE") {
    const s = shares.find((x) => x.token === decodeURIComponent(m[1]));
    if (!s) return fail(res, 404, "not_found", "no such share"), true;
    s.revoked_at ??= new Date().toISOString();
    res.statusCode = 204;
    return res.end(), true;
  }
  if ((m = /^items\/([^/]+)\/renditions$/.exec(rest)) && method === "GET") return send(res, 200, renditions(decodeURIComponent(m[1]))), true;
  return false;
}
