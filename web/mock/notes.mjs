// Notes for the mock server (docs/server-api.md, "Notes"), in memory.
// Seeds: the repo's deploy/seed/notes/*.md (front matter + markdown; MOCK_NOTES_SEED_DIR
// overrides the directory) plus a few samples.
// Chat "salva questo come nota <name>: <text>" in the mock is not wired; use the app or
// POST /v1/notes.

import { randomBytes } from "node:crypto";
import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const MAX_BYTES = 256 * 1024;
const TRASH_DAYS = 30;
const RESERVED = new Set(["trash", "new", "search"]);
const notes = new Map();
const trash = new Map();

const now = () => new Date().toISOString();
const newId = () => `n_${randomBytes(10).toString("hex")}`;

export function slugify(text) {
  const s = String(text || "")
    .normalize("NFKD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 64)
    .replace(/-+$/, "");
  return RESERVED.has(s) ? `${s}-note` : s;
}

const validSlug = (s) => /^[a-z0-9](?:[a-z0-9]|-(?=[a-z0-9])){0,63}$/.test(s) && !RESERVED.has(s);

function freeSlug(base, except) {
  const taken = (s) => [...notes.values()].some((n) => n.slug === s && n.id !== except);
  if (!taken(base)) return base;
  for (let i = 2; ; i++) {
    const s = `${base.slice(0, 64 - String(i).length - 1).replace(/-+$/, "")}-${i}`;
    if (!taken(s)) return s;
  }
}

function normTags(tags) {
  if (tags === undefined) return [];
  if (!Array.isArray(tags)) throw new Error("tags: must be an array of strings");
  const out = [];
  for (const raw of tags) {
    const t = String(raw).trim().replace(/^#/, "").replace(/\s+/g, " ").toLowerCase();
    if (t && !out.includes(t)) out.push(t);
  }
  if (out.length > 16) throw new Error("tags: at most 16");
  return out;
}

function add(n, at = now()) {
  const note = { id: newId(), slug: n.slug, title: n.title, content: n.content ?? "", tags: normTags(n.tags), source: n.source ?? "owner", pinned: !!n.pinned, createdAt: at, updatedAt: at };
  notes.set(note.id, note);
  return note;
}

function seedFromRepo() {
  const env = process.env.MOCK_NOTES_SEED_DIR;
  const dir = env ? `${env.replace(/\/+$/, "")}/` : fileURLToPath(new URL("../../deploy/seed/notes/", import.meta.url));
  let files = [];
  try {
    files = readdirSync(dir).filter((f) => f.endsWith(".md"));
  } catch {
    return;
  }
  for (const f of files) {
    const text = readFileSync(dir + f, "utf8");
    const m = /^---\n([\s\S]*?)\n---\n([\s\S]*)$/.exec(text);
    if (!m) continue;
    const head = Object.fromEntries(m[1].split("\n").map((l) => l.split(/:(.*)/s).map((x) => x.trim())).filter((kv) => kv[0]));
    add({ slug: head.slug, title: head.title, content: m[2].replace(/^\n+/, ""), tags: (head.tags || "").split(",").map((t) => t.trim()).filter(Boolean), source: head.source || "owner", pinned: head.pinned === "true" }, new Date(Date.now() - 50 * 60_000).toISOString());
  }
}

seedFromRepo();
add({ slug: "gift-ideas", title: "Gift ideas for Christmas", source: "lucina", tags: ["family"], content: "Saved by Lucina from the chat.\n\n- **Mamma**: a cookbook (*Il cucchiaio d'argento*)\n- **Marco**: tickets for the match\n- [ ] Check the shop's opening hours" }, new Date(Date.now() - 26 * 3600_000).toISOString());
add({ slug: "rome-weekend", title: "Weekend in Rome — plan", source: "grok", tags: ["travel"], content: "## Friday\nTrain at 08:10 from Milano Centrale.\n\n## Saturday\n1. Galleria Borghese (book at https://www.galleriaborghese.it)\n2. Trastevere for dinner\n\n| Day | Budget |\n|---|--:|\n| Fri | 80 € |\n| Sat | 120 € |" }, new Date(Date.now() - 3 * 86400_000).toISOString());
add({ slug: "reading-list", title: "Reading list", source: "claude-code", tags: ["books"], content: "- *The Pragmatic Programmer*\n- *Designing Data-Intensive Applications*\n\n> Read one chapter a day." }, new Date(Date.now() - 6 * 86400_000).toISOString());

function fold(s) {
  return String(s || "").normalize("NFKD").replace(/[̀-ͯ]/g, "").toLowerCase();
}

function summary(n, q) {
  const flat = n.content.replace(/\s+/g, " ").trim();
  let excerpt = flat.slice(0, 220) + (flat.length > 220 ? "…" : "");
  if (q) {
    const at = fold(flat).indexOf(q);
    if (at > 0) excerpt = `…${flat.slice(Math.max(0, at - 60), at + 160)}…`;
  }
  const { content, ...rest } = n;
  const out = { ...rest, size: Buffer.byteLength(content), excerpt };
  if (n.deletedAt) out.purgeAfter = new Date(Date.parse(n.deletedAt) + TRASH_DAYS * 86400_000).toISOString();
  return out;
}

const find = (key) => {
  const k = decodeURIComponent(key).toLowerCase();
  return notes.get(k) ?? [...notes.values()].find((n) => n.slug === k);
};

/** Handles /v1/notes*; returns true when it answered. */
export async function handleNotes(req, res, { path, method, url, send, fail, readJson }) {
  if (!path.startsWith("/v1/notes")) return false;
  const body = ["POST", "PATCH"].includes(method) ? await readJson(req, 800 * 1024).catch(() => ({})) : null;
  const parts = path.split("/").filter(Boolean); // v1 notes [key] [append]

  if (parts.length === 2 && method === "GET") {
    const q = fold(url.searchParams.get("q") ?? "").trim();
    const tag = (url.searchParams.get("tag") ?? "").toLowerCase();
    const list = [...notes.values()]
      .filter((n) => !tag || n.tags.includes(tag))
      .filter((n) => !q || fold(`${n.title} ${n.slug} ${n.tags.join(" ")} ${n.content}`).includes(q))
      .sort((a, b) => Number(b.pinned) - Number(a.pinned) || b.updatedAt.localeCompare(a.updatedAt));
    send(res, 200, { notes: list.map((n) => summary(n, q)) });
    return true;
  }
  if (parts.length === 2 && method === "POST") {
    const title = String(body?.title ?? "").trim();
    if (!title) return fail(res, 400, "bad_request", "title: must not be empty"), true;
    const content = String(body?.content ?? "");
    if (Buffer.byteLength(content) > MAX_BYTES) return fail(res, 413, "note_too_large", "a note holds at most 256 KB of text"), true;
    let slug;
    if (body?.slug) {
      slug = String(body.slug).toLowerCase();
      if (!validSlug(slug)) return fail(res, 400, "bad_request", "slug: lower-case letters, digits and single '-'"), true;
      if ([...notes.values()].some((n) => n.slug === slug)) return fail(res, 409, "slug_taken", `a note named "${slug}" exists`), true;
    } else slug = freeSlug(slugify(title) || "note");
    let tags;
    try {
      tags = normTags(body?.tags);
    } catch (e) {
      return fail(res, 400, "bad_request", e.message), true;
    }
    const n = add({ slug, title, content, tags, source: body?.source || "owner", pinned: body?.pinned });
    send(res, 201, n);
    return true;
  }
  if (parts.length === 3 && parts[2] === "trash" && method === "GET") {
    send(res, 200, { notes: [...trash.values()].sort((a, b) => b.deletedAt.localeCompare(a.deletedAt)).map((n) => summary(n)), days: TRASH_DAYS });
    return true;
  }
  if (parts.length === 5 && parts[2] === "trash" && parts[4] === "restore" && method === "POST") {
    const n = trash.get(parts[3]);
    if (!n) return fail(res, 404, "note_not_found", "no such note"), true;
    trash.delete(n.id);
    delete n.deletedAt;
    n.slug = freeSlug(n.slug);
    n.updatedAt = now();
    notes.set(n.id, n);
    send(res, 200, n);
    return true;
  }
  const n = parts[2] ? find(parts[2]) : null;
  if (parts.length === 3 && method === "GET") {
    if (!n) return fail(res, 404, "note_not_found", "no such note"), true;
    send(res, 200, n);
    return true;
  }
  if (parts.length === 3 && method === "PATCH") {
    if (!n) return fail(res, 404, "note_not_found", "no such note"), true;
    if (body?.baseUpdatedAt && body.baseUpdatedAt !== n.updatedAt) {
      return fail(res, 409, "conflict", "the note changed since you opened it; reload and try again"), true;
    }
    if (body?.content !== undefined && Buffer.byteLength(String(body.content)) > MAX_BYTES) return fail(res, 413, "note_too_large", "too large"), true;
    if (body?.title !== undefined) n.title = String(body.title).trim() || n.title;
    if (body?.content !== undefined) n.content = String(body.content);
    if (body?.tags !== undefined) n.tags = normTags(body.tags);
    if (body?.pinned !== undefined) n.pinned = !!body.pinned;
    n.updatedAt = now();
    send(res, 200, n);
    return true;
  }
  if (parts.length === 3 && method === "DELETE") {
    if (!n) return fail(res, 404, "note_not_found", "no such note"), true;
    notes.delete(n.id);
    n.deletedAt = now();
    trash.set(n.id, n);
    send(res, 200, { deleted: true, id: n.id, slug: n.slug, deletedAt: n.deletedAt });
    return true;
  }
  if (parts.length === 4 && parts[3] === "append" && method === "POST") {
    if (!n) return fail(res, 404, "note_not_found", "no such note"), true;
    const add2 = String(body?.content ?? "");
    const sep = n.content ? String(body?.separator ?? "") : "";
    if (Buffer.byteLength(n.content + sep + add2) > MAX_BYTES) return fail(res, 413, "note_too_large", "too large"), true;
    n.content += sep + add2;
    n.updatedAt = now();
    send(res, 200, n);
    return true;
  }
  fail(res, 404, "not_found", "Not found.");
  return true;
}
