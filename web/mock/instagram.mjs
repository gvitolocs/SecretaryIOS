// Instagram plugin for the mock server (docs/instagram.md, /v1/instagram). In memory, synthetic
// pictures (SVG gradients) and one generated test video (ffmpeg, if installed; else video slides 404).
//
//   natgeo   stories (image + video), 4 highlights, 12 posts (+6 on "P2"), 20 reels (2 pages), verified
//   nasa     no stories, no highlights, 12 posts whose cursor page is "exhausted" (the real backend ignores cursors)
//   private  a private account with nothing public
//   slow     answers 429 with Retry-After: 12 (the rate-limit message)
//   anything else: 404 not_found
// Save: POST /v1/instagram/save answers every item as saved; a URL saved twice is a duplicate.

import { execFile } from "node:child_process";
import { createReadStream, existsSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const NOW = Math.floor(Date.now() / 1000);
const DAY = 86400;

/** Signed-looking media URL (the mock doesn't check it): kind-seed picks the picture. */
const media = (kind, seed) => `/v1/instagram/s/9999999999/mock/${kind}-${seed}`;

const CAPTIONS = [
  "Golden hour over the savanna. The herd moved at dusk, and so did we.",
  "Photograph by @explorer. Swipe for the full story.",
  "Behind the scenes with our field team in the Arctic.",
  "A reminder to look up tonight: the sky has plans.",
  "Hands that carry the harvest. Link in bio.",
  "Ten years of reefs, in one frame.",
];

function post(user, n, shape) {
  const id = `D${user.slice(0, 3)}${String(n).padStart(2, "0")}Xq${(n * 7919).toString(36).slice(0, 3)}`.slice(0, 11).padEnd(11, "x");
  const base = {
    id,
    takenAt: NOW - n * DAY * 1.5,
    caption: CAPTIONS[n % CAPTIONS.length],
    likeCount: 12000 + n * 311,
    commentCount: 90 + n * 7,
  };
  if (shape === "video") {
    return { ...base, kind: "video", url: media("video", n), thumbUrl: media("img", n), permalink: `https://www.instagram.com/reel/${id}/` };
  }
  if (shape === "carousel") {
    const kinds = ["image", "image", "video", "image"];
    const children = kinds.map((k, i) => ({
      id: `${id}_${i}`,
      kind: k,
      url: media(k === "video" ? "video" : "img", n * 10 + i),
      thumbUrl: media("img", n * 10 + i),
    }));
    return { ...base, kind: "carousel", url: children[0].url, thumbUrl: children[0].thumbUrl, children, permalink: `https://www.instagram.com/p/${id}/` };
  }
  return { ...base, kind: "image", url: media("img", n), thumbUrl: media("img", n), permalink: `https://www.instagram.com/p/${id}/` };
}

const SHAPES = ["image", "video", "carousel", "image", "image", "video", "carousel", "image", "video", "image", "carousel", "image"];
const postsPage = (user, from, count) => Array.from({ length: count }, (_, i) => post(user, from + i, SHAPES[(from + i) % SHAPES.length]));

function reel(user, n) {
  const p = post(user, n, "video");
  return { ...p, takenAt: null, thumbUrl: media("story", n + 40) };
}

function story(n, shape) {
  return {
    id: `40159631736842${50000 + n}`,
    kind: shape,
    url: media(shape === "video" ? "video" : "story", n + 100),
    thumbUrl: media("story", n + 100),
    takenAt: NOW - n * 3000,
    caption: null,
    likeCount: 0,
    commentCount: 0,
  };
}

const PROFILES = {
  natgeo: {
    profile: { username: "natgeo", fullName: "National Geographic", biography: "Step into wonder.\nOur planet, through the eyes of explorers and photographers.", picUrl: media("avatar", 1), profileUrl: "https://www.instagram.com/natgeo/", mediaCount: 32041, followerCount: 268_000_000, followingCount: 180, isPrivate: false, hasStories: true, isVerified: true, userId: "787132", externalUrl: "https://on.natgeo.com/bestoftheworld" },
    highlights: [
      { id: "highlight:18023864549864045", title: "Best of the World", coverUrl: media("avatar", 11), mediaCount: 4 },
      { id: "highlight:18023864549864046", title: "Wild", coverUrl: media("avatar", 12), mediaCount: 3 },
      { id: "highlight:18023864549864047", title: "Travel", coverUrl: media("avatar", 13), mediaCount: 3 },
      { id: "highlight:18023864549864048", title: "Behind the lens", coverUrl: media("avatar", 14), mediaCount: 2 },
    ],
    stories: [story(1, "image"), story(2, "video"), story(3, "image"), story(4, "image"), story(5, "video")],
    posts: (cursor) => (cursor === "P2" ? { items: postsPage("natgeo", 12, 6), nextCursor: null } : { items: postsPage("natgeo", 0, 12), nextCursor: "P2" }),
    reels: (cursor) => (cursor === "R2" ? { items: Array.from({ length: 8 }, (_, i) => reel("natgeo", 20 + i)), nextCursor: null } : { items: Array.from({ length: 12 }, (_, i) => reel("natgeo", 8 + i)), nextCursor: "R2" }),
  },
  nasa: {
    profile: { username: "nasa", fullName: "NASA", biography: "Exploring the universe and our home planet.", picUrl: media("avatar", 2), profileUrl: "https://www.instagram.com/nasa/", mediaCount: 4981, followerCount: 97_500_000, followingCount: 76, isPrivate: false, hasStories: false, isVerified: true, userId: "528817151", externalUrl: "https://www.nasa.gov" },
    highlights: [],
    stories: [],
    posts: (cursor) => (cursor ? { items: [], nextCursor: null, exhausted: true } : { items: postsPage("nasa", 30, 12), nextCursor: "N2" }),
    reels: () => ({ items: [], nextCursor: null }),
  },
  private: {
    profile: { username: "private", fullName: "A private account", biography: null, picUrl: media("avatar", 3), profileUrl: "https://www.instagram.com/private/", mediaCount: 41, followerCount: 212, followingCount: 301, isPrivate: true, hasStories: false, isVerified: false, userId: "9", externalUrl: null },
    highlights: [],
    stories: [],
    posts: () => ({ items: [], nextCursor: null }),
    reels: () => ({ items: [], nextCursor: null }),
  },
};

const HIGHLIGHT_ITEMS = (id) => {
  const n = Number(id.slice(-1)) || 5;
  return Array.from({ length: n % 2 ? 3 : 4 }, (_, i) => story(i + n, i % 3 === 1 ? "video" : "image"));
};

/** Profiles the owner opened, newest first. */
let recent = [
  { username: "nasa", fullName: "NASA", isVerified: true, picUrl: media("avatar", 2), openedAt: NOW - 3600 },
  { username: "natgeo", fullName: "National Geographic", isVerified: true, picUrl: media("avatar", 1), openedAt: NOW - 86400 },
];
const saved = new Set();
let counter = 0;

// ---- pictures and the video --------------------------------------------------------------------

function svg(kind, seed) {
  const [w, h] = kind === "story" ? [720, 1280] : kind === "avatar" ? [200, 200] : [1080, 1350];
  const hue = (seed * 47) % 360;
  const a = `hsl(${hue} 65% 52%)`;
  const b = `hsl(${(hue + 60) % 360} 70% 28%)`;
  const label = kind === "avatar" ? "" : `<text x="${w * 0.06}" y="${h * 0.1}" font-family="monospace" font-size="${Math.round(w * 0.06)}" fill="#fff" fill-opacity=".85">#${seed}</text>`;
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${a}"/><stop offset="1" stop-color="${b}"/></linearGradient></defs><rect width="${w}" height="${h}" fill="url(#g)"/><path d="M0 ${h} L${w * 0.35} ${h * 0.6} L${w * 0.58} ${h * 0.82} L${w * 0.8} ${h * 0.64} L${w} ${h} Z" fill="rgba(0,0,0,.25)"/><circle cx="${w * 0.74}" cy="${h * 0.24}" r="${Math.min(w, h) * 0.1}" fill="rgba(255,255,255,.6)"/>${label}</svg>`;
}

/** The test clip is made once, at start-up, so a loaded machine can't time a request out. */
let videoFile = "";
const videoReady = new Promise((resolve) => {
  const out = join(tmpdir(), `ig-mock-${process.pid}.webm`);
  execFile(
    "ffmpeg",
    ["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=duration=4:size=360x640:rate=15", "-c:v", "libvpx", "-deadline", "realtime", "-cpu-used", "8", "-b:v", "300k", "-an", out],
    (err) => {
      videoFile = err ? "" : out;
      resolve();
    },
  );
});

async function serveMedia(req, res, path) {
  const token = path.split("/").pop() ?? "";
  const [kind, seedText] = token.split("-");
  const seed = Number(seedText) || 0;
  if (kind === "video") {
    await videoReady;
    const file = videoFile;
    if (!file || !existsSync(file)) {
      res.writeHead(404);
      return res.end();
    }
    const size = statSync(file).size;
    const range = /^bytes=(\d*)-(\d*)$/.exec(req.headers.range ?? "");
    if (range) {
      const start = range[1] ? Number(range[1]) : Math.max(0, size - Number(range[2]));
      const end = range[1] && range[2] ? Math.min(Number(range[2]), size - 1) : size - 1;
      res.writeHead(206, { "Content-Type": "video/webm", "Accept-Ranges": "bytes", "Content-Range": `bytes ${start}-${end}/${size}`, "Content-Length": end - start + 1 });
      return createReadStream(file, { start, end }).pipe(res);
    }
    res.writeHead(200, { "Content-Type": "video/webm", "Accept-Ranges": "bytes", "Content-Length": size });
    return createReadStream(file).pipe(res);
  }
  res.writeHead(200, { "Content-Type": "image/svg+xml", "Cache-Control": "private, max-age=3600" });
  res.end(svg(kind, seed));
}

// ---- routes ------------------------------------------------------------------------------------

/** The signed media route: no session, like p1's /v1/instagram/s/*. */
export function instagramMedia(req, res, path) {
  if (!path.startsWith("/v1/instagram/s/")) return false;
  void serveMedia(req, res, path);
  return true;
}

export async function handleInstagram(req, res, { path, method, url, send, readJson }) {
  if (!path.startsWith("/v1/instagram")) return false;
  const ok = (data) => send(res, 200, { ok: true, data });
  const err = (status, code, headers = {}, extra = {}) => send(res, status, { ok: false, error: code, ...extra }, headers);
  const rest = path.slice("/v1/instagram".length);
  const name = (prefix) => decodeURIComponent(rest.slice(prefix.length)).toLowerCase();
  const find = (prefix) => {
    const u = name(prefix);
    if (u === "slow") return err(429, "rate_limited", { "Retry-After": "12" }, { retryAfter: 12 }), null;
    const p = PROFILES[u];
    if (!p) return err(404, "not_found"), null;
    return p;
  };
  const cursor = url.searchParams.get("cursor");

  if (rest === "/status" && method === "GET") return ok({ enabled: true, rateLimitPerSecond: 1 }), true;
  if (rest === "/recent" && method === "GET") return ok({ items: recent }), true;
  if (rest.startsWith("/profile/") && method === "GET") {
    const p = find("/profile/");
    if (!p) return true;
    recent = [{ username: p.profile.username, fullName: p.profile.fullName, isVerified: p.profile.isVerified, picUrl: p.profile.picUrl, openedAt: Math.floor(Date.now() / 1000) }, ...recent.filter((r) => r.username !== p.profile.username)].slice(0, 30);
    return ok({ profile: p.profile, posts: p.posts(null), highlights: p.highlights }), true;
  }
  if (rest.startsWith("/posts/") && method === "GET") {
    const p = find("/posts/");
    return p && ok(p.posts(cursor)), true;
  }
  if (rest.startsWith("/reels/") && method === "GET") {
    const p = find("/reels/");
    return p && ok(p.reels(cursor)), true;
  }
  if (rest.startsWith("/stories/") && method === "GET") {
    const p = find("/stories/");
    return p && ok({ items: p.stories, nextCursor: null }), true;
  }
  if (rest === "/highlights/items" && method === "GET") {
    const id = url.searchParams.get("id") ?? "";
    if (!id.startsWith("highlight:")) return err(400, "bad_request"), true;
    return ok({ items: HIGHLIGHT_ITEMS(id), nextCursor: null }), true;
  }
  if (rest.startsWith("/highlights/") && method === "GET") {
    const p = find("/highlights/");
    return p && ok({ items: p.highlights }), true;
  }
  if (rest === "/resolve" && method === "GET") {
    const target = url.searchParams.get("url") ?? "";
    if (!/^https?:\/\/(www\.)?instagram\.com\/([^/]+\/)?(p|reel|reels|tv)\/[\w-]+/i.test(target)) return err(400, "bad_request"), true;
    const item = /reel/i.test(target) ? post("natgeo", 3, "video") : post("natgeo", 2, "carousel");
    return ok({ items: [item], nextCursor: null }), true;
  }
  if (rest === "/save" && method === "POST") {
    const body = await readJson(req);
    const items = Array.isArray(body.items) ? body.items : [];
    if (items.length === 0 || items.length > 50) return err(400, "bad_request"), true;
    const result = { saved: [], failed: [] };
    items.forEach((it, index) => {
      if (typeof it.url !== "string" || !it.url.startsWith("/v1/instagram/s/")) return result.failed.push({ index, error: "bad_url" });
      const duplicate = saved.has(it.url);
      saved.add(it.url);
      result.saved.push({ index, itemId: `itm_mock_${++counter}`, duplicate });
    });
    return ok(result), true;
  }
  return err(404, "not_found"), true;
}
