// Notes for the mock server (docs/server-api.md, "Notes", and the Keep contract), in memory.
// Seeds: the repo's deploy/seed/notes/*.md (front matter + markdown; MOCK_NOTES_SEED_DIR
// overrides the directory) plus invented Keep-style notes (labels, colours, lists, an image,
// archived and trashed notes) so /keep looks lived in.
// Keep adds to the same store: colour, archive, checklists, attachments, reminders, positions,
// the changes feed, labels, batch writes, the trash and the Google Keep import.
// Chat "salva questo come nota <name>: <text>" in the mock is not wired; use the app or
// POST /v1/notes.

import { createHash, randomBytes } from "node:crypto";
import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { crc32, deflateSync, inflateRawSync } from "node:zlib";

const MAX_BYTES = 256 * 1024;
const TRASH_DAYS = 30;
const RESERVED = new Set(["trash", "new", "search", "labels", "changes", "attachments", "import", "batch"]);
const COLORS = ["default", "red", "orange", "yellow", "green", "teal", "blue", "cerulean", "purple", "pink", "brown", "gray"];
const RECURRENCES = ["daily", "weekly", "monthly", "yearly"];
const ATTACHMENT_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp", "image/heic", "image/heif", "audio/3gpp", "audio/mp4", "audio/aac", "audio/mpeg", "audio/ogg", "audio/webm", "audio/x-m4a", "application/pdf"];
const EXT = { "image/png": "png", "image/jpeg": "jpg", "image/gif": "gif", "image/webp": "webp", "image/heic": "heic", "image/heif": "heif", "audio/3gpp": "3gp", "audio/mp4": "m4a", "audio/aac": "aac", "audio/mpeg": "mp3", "audio/ogg": "ogg", "audio/webm": "webm", "audio/x-m4a": "m4a", "application/pdf": "pdf" };

const notes = new Map();
const trash = new Map();
const files = new Map(); // "<sha256>.<ext>" -> { bytes, mimetype }
const registry = new Map(); // tag -> { tag, name, createdAt }
const tombstones = []; // { id, rev }
const importedKeys = new Set();
let storeRev = 0;

const now = () => new Date().toISOString();
const newId = () => `n_${randomBytes(10).toString("hex")}`;
const itemId = () => `i_${randomBytes(6).toString("hex")}`;
const nextRev = () => ++storeRev;
const stamp = (n) => {
  n.rev = nextRev();
  return n;
};

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
  const taken = (s) => [...notes.values(), ...trash.values()].some((n) => n.slug === s && n.id !== except);
  if (!taken(base)) return base;
  for (let i = 2; ; i++) {
    const s = `${base.slice(0, 64 - String(i).length - 1).replace(/-+$/, "")}-${i}`;
    if (!taken(s)) return s;
  }
}

const normTag = (raw) => String(raw).trim().replace(/^#/, "").replace(/\s+/g, " ").trim().toLowerCase();
const cleanName = (raw) => String(raw).trim().replace(/^#/, "").replace(/\s+/g, " ").trim();

function normTags(tags) {
  if (tags === undefined) return [];
  if (!Array.isArray(tags)) throw new Error("tags: must be an array of strings");
  const out = [];
  for (const raw of tags) {
    const t = normTag(raw);
    if (!t) continue;
    if (t.length > 50 || /[\u0000-\u001f,]/.test(t)) throw new Error("tags: at most 50 characters, no commas");
    if (!out.includes(t)) out.push(t);
    if (!registry.has(t)) registry.set(t, { tag: t, name: cleanName(raw), createdAt: now() });
  }
  if (out.length > 50) throw new Error("tags: at most 50");
  return out;
}

const itemsToContent = (items) => items.map((i) => `${i.indent === 1 ? "  " : ""}- [${i.checked ? "x" : " "}] ${i.text}`).join("\n");

function contentToItems(content) {
  const items = [];
  for (const raw of String(content).split(/\r?\n/)) {
    if (!raw.trim()) continue;
    const m = /^(\s*)[-*]\s+\[([ xX])\]\s?(.*)$/.exec(raw);
    if (m) items.push({ id: itemId(), text: m[3].trim(), checked: m[2] !== " ", indent: m[1].length >= 2 ? 1 : 0 });
    else items.push({ id: itemId(), text: raw.trim(), checked: false, indent: 0 });
  }
  return items;
}

function normItems(items) {
  if (!Array.isArray(items)) throw new Error("items: must be an array");
  return items.map((i) => ({ id: typeof i.id === "string" && i.id ? i.id : itemId(), text: String(i.text ?? ""), checked: !!i.checked, indent: i.indent === 1 ? 1 : 0 }));
}

function add(n, at = now()) {
  const ms = Date.parse(at);
  const note = {
    id: newId(),
    slug: n.slug,
    title: n.title ?? "",
    content: n.content ?? "",
    tags: normTags(n.tags),
    color: COLORS.includes(n.color) ? n.color : "default",
    archived: !!n.archived,
    attachments: n.attachments ?? [],
    annotations: n.annotations ?? [],
    collaborators: n.collaborators ?? [],
    position: typeof n.position === "number" ? n.position : ms,
    source: n.source ?? "owner",
    pinned: !!n.pinned,
    createdAt: at,
    updatedAt: n.updatedAt ?? at,
  };
  if (n.items) {
    note.items = normItems(n.items);
    note.content = itemsToContent(note.items);
  }
  if (n.reminder) note.reminder = n.reminder;
  stamp(note);
  if (n.deletedAt) {
    note.deletedAt = n.deletedAt;
    trash.set(note.id, note);
  } else notes.set(note.id, note);
  return note;
}

// ---------------------------------------------------------------- seed

/** A small PNG (a bar chart on a dark panel) so the image card has something to show. */
function makePng(w, h, bars, bg = [28, 30, 36], fg = [138, 180, 248]) {
  const raw = Buffer.alloc((w * 3 + 1) * h);
  const bw = Math.floor(w / (bars.length * 2 + 1));
  for (let y = 0; y < h; y++) {
    const row = y * (w * 3 + 1);
    raw[row] = 0;
    for (let x = 0; x < w; x++) {
      let c = bg;
      const k = Math.floor(x / bw);
      if (k % 2 === 1 && Math.floor(k / 2) < bars.length) {
        const bh = Math.floor(bars[Math.floor(k / 2)] * (h - 30));
        if (y > h - 12 - bh && y < h - 12) c = fg.map((v, i) => Math.min(255, v + Math.floor((y / h) * 40) - 20 + (i === 0 ? 10 : 0)));
      }
      if (y === h - 12 && x > 6 && x < w - 6) c = [90, 96, 108];
      raw[row + 1 + x * 3] = c[0];
      raw[row + 2 + x * 3] = c[1];
      raw[row + 3 + x * 3] = c[2];
    }
  }
  const chunk = (type, data) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(td) >>> 0);
    return Buffer.concat([len, td, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8;
  ihdr[9] = 2;
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
}

function putFile(bytes, mimetype, name) {
  const sha = createHash("sha256").update(bytes).digest("hex");
  const file = `${sha}.${EXT[mimetype] ?? "bin"}`;
  files.set(file, { bytes, mimetype });
  const a = { id: sha, file, mimetype, size: bytes.length, createdAt: now() };
  if (name) a.name = name;
  return a;
}

function seedFromRepo() {
  const env = process.env.MOCK_NOTES_SEED_DIR;
  const dir = env ? `${env.replace(/\/+$/, "")}/` : fileURLToPath(new URL("../../deploy/seed/notes/", import.meta.url));
  let list = [];
  try {
    list = readdirSync(dir).filter((f) => f.endsWith(".md"));
  } catch {
    return;
  }
  for (const f of list) {
    const text = readFileSync(dir + f, "utf8");
    const m = /^---\n([\s\S]*?)\n---\n([\s\S]*)$/.exec(text);
    if (!m) continue;
    const head = Object.fromEntries(m[1].split("\n").map((l) => l.split(/:(.*)/s).map((x) => x.trim())).filter((kv) => kv[0]));
    add({ slug: head.slug, title: head.title, content: m[2].replace(/^\n+/, ""), tags: (head.tags || "").split(",").map((t) => t.trim()).filter(Boolean), source: head.source || "owner", pinned: head.pinned === "true" }, new Date(Date.now() - 50 * 60_000).toISOString());
  }
}

const LABELS = ["Chill", "Donne", "EUNE", "film", "Fisica 2", "GodLevel", "health tips", "Hope", "INFP", "Ingegneria", "Instagram", "JOB"];
for (const name of LABELS) registry.set(normTag(name), { tag: normTag(name), name, createdAt: now() });

seedFromRepo();

(function seedKeep() {
  const H = 3600_000;
  const base = Date.now();
  let age = 0;
  const at = () => new Date(base - 2 * H - (age += 5) * 3600_000 * 0.9).toISOString();
  const keep = (n) => add({ source: "web", slug: freeSlug(slugify(n.title || n.content || "note") || "note"), ...n }, n.at ?? at());

  keep({
    title: "Revision plan",
    pinned: true,
    color: "teal",
    tags: ["GodLevel"],
    content:
      "Split the course into **small, finite blocks**.\nFor example, with **12 days left** and **3 big topics**, do not spread the time evenly. Use something like:\n\n**Days 1–5:** read each topic once, make a one-page map\n**Days 6–8:** problems and past exams, mark the weak spots\n**Days 9–10:** second pass on the weak spots only\n**Days 11–12:** mock exam, sleep, light review\n\nIf a block overruns, cut the *next* block, never the sleep.",
  });
  keep({
    title: "Chat setup",
    color: "default",
    tags: ["Ingegneria"],
    content: "# Local model\nrun the small model with the default flags\n\n# Endpoint\nBase URL: http://127.0.0.1:9000/v1\nModel: any name works\nKey: not needed\n\n# Notes\nKeep the context short; restart the server when it gets slow.",
  });
  keep({
    title: "Active recall",
    tags: ["GodLevel"],
    content: "Test yourself before rereading. Close the book, explain the idea out loud, then check what you missed.",
  });
  const img = putFile(makePng(480, 300, [0.35, 0.5, 0.42, 0.7, 0.62, 0.9]), "image/png", "chart.png");
  keep({
    title: "growth sketch",
    tags: ["film"],
    attachments: [img],
    content: "The curve is flat for years, then bends.\nThe bend matters more than the average.\nWrite the two numbers on the first page.",
  });
  keep({
    title: "Groceries",
    tags: ["health tips"],
    color: "green",
    items: [
      { text: "Oats", checked: false },
      { text: "Greek yogurt", checked: false },
      { text: "Spinach", checked: false },
      { text: "Olive oil", checked: true },
      { text: "Lemons", checked: true },
      { text: "Walnuts", checked: true },
    ],
  });
  keep({
    title: "Call the dentist",
    tags: ["JOB"],
    reminder: { at: new Date(base + 20 * H).toISOString(), lucinaId: "rem_mock_1" },
    content: "Ask about the Tuesday slot and the invoice.",
  });
  keep({
    title: "Ideas for the talk",
    color: "red",
    tags: ["Hope"],
    content: "Open with the failure, not the result.\nOne slide, one sentence.\nEnd on the question the audience will ask anyway.",
  });
  keep({
    title: "Packing list",
    color: "orange",
    tags: ["Chill"],
    items: [
      { text: "Passport", checked: false },
      { text: "Charger", checked: false },
      { text: "Rain jacket", checked: false },
      { text: "Book", checked: false },
      { text: "Sunscreen", checked: false },
      { text: "Notebook", checked: false },
      { text: "Headphones", checked: false },
      { text: "Adapter", checked: false },
    ],
  });
  keep({
    title: "Read later",
    color: "default",
    tags: ["Hope", "Instagram"],
    annotations: [{ url: "https://example.org/sleep-and-memory", title: "How sleep shapes what you remember", description: "A long read on consolidation.", source: "example.org" }],
    content: "Saved for the weekend.",
  });
  keep({ title: "", color: "yellow", content: "Buy a bigger desk lamp.\nCheck the colour temperature before paying.", tags: [] });
  keep({ title: "Quote", color: "purple", tags: ["INFP"], content: "*Be patient with the draft.* It is allowed to be bad for a while." });
  keep({ title: "Wiring", color: "cerulean", tags: ["Ingegneria", "EUNE"], content: "Pin 4 to the **red** wire, pin 6 to the black one.\nCheck continuity before power." });
  keep({ title: "Gift ideas", color: "pink", tags: ["Donne"], content: "A scarf in the green she liked.\nThe small camera strap.\nA print from the market." });
  keep({ title: "Fisica 2 — formulas", color: "brown", tags: ["Fisica 2"], content: "<u>Gauss</u>: flux = enclosed charge / epsilon\nBiot–Savart for the loop\nCheck units twice." });
  keep({ title: "Mood", color: "gray", tags: ["Chill"], content: "Walk after lunch.\nNo screens after eleven." });
  keep({
    title: "Long notes from the lecture",
    tags: ["Ingegneria"],
    content: Array.from({ length: 34 }, (_, i) => `Point ${i + 1}: a sentence that explains one small step and why it follows from the previous one.`).join("\n"),
  });
  keep({ title: "Old ideas", archived: true, tags: ["JOB"], content: "Replace the weekly meeting with a written update." });
  keep({ title: "Archived list", archived: true, items: [{ text: "Return the book", checked: true }, { text: "Cancel the old plan", checked: true }] });
  keep({ title: "Throwaway", deletedAt: new Date(base - 3 * 86400_000).toISOString(), content: "Not needed anymore." });
  keep({ title: "Draft email", deletedAt: new Date(base - 9 * 86400_000).toISOString(), tags: ["JOB"], content: "Hello, a quick question about the schedule." });
})();

add({ slug: "gift-ideas-xmas", title: "Gift ideas for Christmas", source: "lucina", tags: ["family"], content: "Saved by Lucina from the chat.\n\n- **Mamma**: a cookbook (*Il cucchiaio d'argento*)\n- **Marco**: tickets for the match\n- [ ] Check the shop's opening hours" }, new Date(Date.now() - 26 * 3600_000).toISOString());
add({ slug: "rome-weekend", title: "Weekend in Rome — plan", source: "grok", tags: ["travel"], content: "## Friday\nTrain at 08:10 from Milano Centrale.\n\n## Saturday\n1. Galleria Borghese (book at https://www.galleriaborghese.it)\n2. Trastevere for dinner\n\n| Day | Budget |\n|---|--:|\n| Fri | 80 € |\n| Sat | 120 € |" }, new Date(Date.now() - 3 * 86400_000).toISOString());
add({ slug: "reading-list", title: "Reading list", source: "claude-code", tags: ["books"], content: "- *The Pragmatic Programmer*\n- *Designing Data-Intensive Applications*\n\n> Read one chapter a day." }, new Date(Date.now() - 6 * 86400_000).toISOString());

// ---------------------------------------------------------------- helpers

function fold(s) {
  return String(s || "").normalize("NFKD").replace(/[̀-ͯ]/g, "").toLowerCase();
}

const labelName = (tag) => registry.get(tag)?.name ?? tag;

function summary(n, q) {
  const flat = n.content.replace(/\s+/g, " ").trim();
  let excerpt = flat.slice(0, 220) + (flat.length > 220 ? "…" : "");
  if (q) {
    const at = fold(flat).indexOf(q);
    if (at > 0) excerpt = `…${flat.slice(Math.max(0, at - 60), at + 160)}…`;
  }
  const { content, items, attachments, annotations, collaborators, reminder, ...rest } = n;
  const out = {
    ...rest,
    title: n.title || n.slug,
    size: Buffer.byteLength(content),
    excerpt,
    isList: !!items,
    itemsTotal: items?.length ?? 0,
    itemsDone: items?.filter((i) => i.checked).length ?? 0,
    attachmentCount: attachments.length,
    hasImage: attachments.some((a) => a.mimetype.startsWith("image/")),
    labels: n.tags.map(labelName),
  };
  if (reminder) out.reminderAt = reminder.at;
  if (n.deletedAt) out.purgeAfter = new Date(Date.parse(n.deletedAt) + TRASH_DAYS * 86400_000).toISOString();
  return out;
}

const find = (key) => {
  const k = decodeURIComponent(key).toLowerCase();
  // Live notes only (docs/keep.md): a trashed note is reached through /trash.
  return notes.get(k) ?? [...notes.values()].find((n) => n.slug === k);
};

function readRaw(req, limit) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    req.on("data", (c) => {
      size += c.length;
      if (size > limit) {
        reject(Object.assign(new Error("body too large"), { status: 413 }));
        req.destroy();
        return;
      }
      chunks.push(c);
    });
    req.on("end", () => resolve(Buffer.concat(chunks)));
    req.on("error", reject);
  });
}

function lucinaReminder(n, reminder) {
  const out = { at: new Date(reminder.at).toISOString() };
  if (RECURRENCES.includes(reminder.recurrence)) out.recurrence = reminder.recurrence;
  out.lucinaId = `rem_mock_${randomBytes(4).toString("hex")}`;
  return out;
}

/** Applies the model fields of a POST / PATCH body to a note; throws Error(message) for a 400. */
function applyBody(n, body) {
  if (body.title !== undefined) n.title = String(body.title).trim();
  if (body.color !== undefined) {
    if (!COLORS.includes(body.color)) throw new Error("color: unknown colour");
    n.color = body.color;
  }
  if (body.pinned !== undefined) n.pinned = !!body.pinned;
  if (body.archived !== undefined) n.archived = !!body.archived;
  if (body.position !== undefined) {
    if (typeof body.position !== "number" || !Number.isFinite(body.position)) throw new Error("position: must be a number");
    n.position = body.position;
  }
  if (body.collaborators !== undefined) n.collaborators = (Array.isArray(body.collaborators) ? body.collaborators : []).map(String).filter(Boolean);
  if (body.annotations !== undefined) n.annotations = Array.isArray(body.annotations) ? body.annotations.filter((a) => a && a.url) : [];
  if (body.tags !== undefined || body.labels !== undefined) n.tags = normTags(body.labels ?? body.tags);
  if (body.content !== undefined) {
    if (Buffer.byteLength(String(body.content)) > MAX_BYTES) throw Object.assign(new Error("a note holds at most 256 KB of text"), { status: 413, code: "note_too_large" });
    n.content = String(body.content);
    if (n.items && body.items === undefined) {
      n.items = contentToItems(n.content);
      n.content = itemsToContent(n.items);
    }
  }
  if (body.items !== undefined) {
    n.items = normItems(body.items);
    n.content = itemsToContent(n.items);
  }
  if (body.convert === "text" && n.items) {
    n.content = n.items.map((i) => i.text).join("\n");
    delete n.items;
  } else if (body.convert === "list" && !n.items) {
    n.items = contentToItems(n.content);
    n.content = itemsToContent(n.items);
  }
  if (body.reminder === null) delete n.reminder;
  else if (body.reminder !== undefined) {
    if (!body.reminder || Number.isNaN(Date.parse(body.reminder.at))) throw new Error("reminder.at: must be an RFC 3339 time");
    n.reminder = lucinaReminder(n, body.reminder);
  }
  if (!n.title && !n.content && !(n.items && n.items.length) && !n.attachments.length) throw new Error("title: must not be empty");
}

function purge(n) {
  notes.delete(n.id);
  trash.delete(n.id);
  tombstones.push({ id: n.id, rev: nextRev() });
  if (n.import) importedKeys.add(n.import.key);
  for (const a of n.attachments) if (![...notes.values(), ...trash.values()].some((m) => m.attachments.some((x) => x.file === a.file))) files.delete(a.file);
}

function trashNote(n) {
  notes.delete(n.id);
  n.deletedAt = now();
  stamp(n);
  trash.set(n.id, n);
}

function restoreNote(n) {
  trash.delete(n.id);
  delete n.deletedAt;
  n.slug = freeSlug(n.slug, n.id);
  n.updatedAt = now();
  stamp(n);
  notes.set(n.id, n);
}

const labelOut = (l) => ({ tag: l.tag, name: l.name, count: [...notes.values()].filter((n) => n.tags.includes(l.tag)).length });

function allLabels() {
  const out = new Map(registry);
  for (const n of [...notes.values()]) for (const t of n.tags) if (!out.has(t)) out.set(t, { tag: t, name: t });
  return [...out.values()].map(labelOut).sort((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase()));
}

// ---------------------------------------------------------------- zip (the Google Keep import)

function listZip(buf) {
  let eocd = -1;
  for (let i = buf.length - 22; i >= Math.max(0, buf.length - 65557); i--) {
    if (buf.readUInt32LE(i) === 0x06054b50) {
      eocd = i;
      break;
    }
  }
  if (eocd < 0) throw Object.assign(new Error("not a zip file"), { status: 400, code: "bad_zip" });
  const count = buf.readUInt16LE(eocd + 10);
  let p = buf.readUInt32LE(eocd + 16);
  const out = [];
  for (let k = 0; k < count && buf.readUInt32LE(p) === 0x02014b50; k++) {
    const nameLen = buf.readUInt16LE(p + 28);
    const extraLen = buf.readUInt16LE(p + 30);
    const commentLen = buf.readUInt16LE(p + 32);
    out.push({ method: buf.readUInt16LE(p + 10), csize: buf.readUInt32LE(p + 20), usize: buf.readUInt32LE(p + 24), offset: buf.readUInt32LE(p + 42), name: buf.toString("utf8", p + 46, p + 46 + nameLen) });
    p += 46 + nameLen + extraLen + commentLen;
  }
  return out;
}

function readZipEntry(buf, e) {
  const nameLen = buf.readUInt16LE(e.offset + 26);
  const extraLen = buf.readUInt16LE(e.offset + 28);
  const start = e.offset + 30 + nameLen + extraLen;
  const data = buf.subarray(start, start + e.csize);
  return e.method === 8 ? inflateRawSync(data) : data;
}

function importGoogleKeep(buf, dryRun) {
  const entries = listZip(buf).filter((e) => !e.name.endsWith("/"));
  const report = {
    dryRun,
    files: { json: 0, html: 0, attachments: 0, ignored: 0 },
    notes: { total: 0, new: 0, updated: 0, unchanged: 0, keptLocalEdits: 0, skippedDeleted: 0, trashed: 0, archived: 0, pinned: 0, lists: 0 },
    labels: { total: 0, new: [] },
    attachments: { total: 0, new: 0, missing: [], bytes: 0 },
    errors: [],
    sample: [],
  };
  const labelSeen = new Set();
  const names = new Set(entries.map((e) => e.name));
  for (const e of entries) {
    if (/\/Keep\/[^/]+\.json$/i.test(e.name) || /(^|\/)Keep\/[^/]+\.json$/i.test(e.name)) {
      let j;
      try {
        j = JSON.parse(readZipEntry(buf, e).toString("utf8"));
        if (!j.createdTimestampUsec) throw new Error("not a Keep note");
      } catch (err) {
        report.files.json++;
        report.errors.push({ file: e.name, error: String(err.message ?? err) });
        continue;
      }
      report.files.json++;
      report.notes.total++;
      const key = `gkeep:${j.createdTimestampUsec}`;
      if (importedKeys.has(key)) {
        report.notes.unchanged++;
        continue;
      }
      report.notes.new++;
      if (j.isTrashed) report.notes.trashed++;
      if (j.isArchived) report.notes.archived++;
      if (j.isPinned) report.notes.pinned++;
      if (j.listContent) report.notes.lists++;
      const labels = (j.labels ?? []).map((l) => l.name);
      for (const l of labels) {
        if (!labelSeen.has(l)) {
          labelSeen.add(l);
          report.labels.total++;
          if (!registry.has(normTag(l))) report.labels.new.push(l);
        }
      }
      for (const a of j.attachments ?? []) {
        report.attachments.total++;
        const dir = e.name.replace(/[^/]+$/, "");
        if (names.has(dir + a.filePath)) {
          report.attachments.new++;
          report.attachments.bytes += entries.find((x) => x.name === dir + a.filePath)?.usize ?? 0;
        } else report.attachments.missing.push(dir + a.filePath);
      }
      if (report.sample.length < 20) report.sample.push({ title: j.title || (j.textContent ?? "").slice(0, 40), labels, color: String(j.color ?? "DEFAULT").toLowerCase(), kind: j.listContent ? "list" : "text" });
      if (!dryRun) {
        importedKeys.add(key);
        const ms = Math.floor(Number(j.createdTimestampUsec) / 1000);
        const at = new Date(ms || Date.now()).toISOString();
        const atts = [];
        for (const a of j.attachments ?? []) {
          const entry = entries.find((x) => x.name === e.name.replace(/[^/]+$/, "") + a.filePath);
          if (entry && ATTACHMENT_TYPES.includes(a.mimetype)) atts.push(putFile(readZipEntry(buf, entry), a.mimetype, a.filePath));
        }
        const n = add(
          {
            slug: freeSlug(slugify(j.title || j.textContent || "note") || "note"),
            title: (j.title ?? "").slice(0, 200),
            content: j.textContent ?? "",
            items: j.listContent ? j.listContent.map((i) => ({ text: i.text, checked: !!i.isChecked })) : undefined,
            color: String(j.color ?? "default").toLowerCase(),
            pinned: j.isPinned,
            archived: j.isArchived,
            tags: labels,
            attachments: atts,
            source: "google-keep",
            position: ms || undefined,
            deletedAt: j.isTrashed ? now() : undefined,
            updatedAt: new Date(Math.floor(Number(j.userEditedTimestampUsec ?? j.createdTimestampUsec) / 1000)).toISOString(),
          },
          at,
        );
        n.import = { source: "google-keep", key, editedUsec: Number(j.userEditedTimestampUsec ?? 0), importedAt: now() };
      }
    } else if (/\.html?$/i.test(e.name)) report.files.html++;
    else if (/\.(jpe?g|png|gif|webp|heic|3gp|m4a|mp3|aac|amr|ogg|pdf)$/i.test(e.name)) report.files.attachments++;
    else report.files.ignored++;
  }
  return report;
}

// ---------------------------------------------------------------- handler

/** Handles /v1/notes*; returns true when it answered. */
export async function handleNotes(req, res, { path, method, url, send, fail, readJson }) {
  if (!path.startsWith("/v1/notes")) return false;
  const parts = path.split("/").filter(Boolean); // v1 notes [key] [sub] ...

  // Raw bodies: attachments and the Takeout zip.
  if (method === "POST" && parts.length === 4 && parts[3] === "attachments") {
    const n = find(parts[2]);
    const bytes = await readRaw(req, 26 * 1024 * 1024).catch(() => null);
    if (!n) return fail(res, 404, "note_not_found", "no such note"), true;
    if (!bytes) return fail(res, 413, "payload_too_large", "at most 25 MB"), true;
    const mimetype = String(req.headers["content-type"] ?? "").split(";")[0].trim();
    if (!ATTACHMENT_TYPES.includes(mimetype)) return fail(res, 415, "unsupported_media_type", `${mimetype || "that type"} is not an allowed attachment`), true;
    if (n.attachments.length >= 50) return fail(res, 400, "bad_request", "at most 50 attachments per note"), true;
    const a = putFile(bytes, mimetype, url.searchParams.get("name") ?? undefined);
    if (!n.attachments.some((x) => x.file === a.file)) n.attachments.push(a);
    n.updatedAt = now();
    stamp(n);
    send(res, 201, n);
    return true;
  }
  if (method === "POST" && path === "/v1/notes/import/google-keep") {
    const bytes = await readRaw(req, 2 * 1024 * 1024 * 1024).catch(() => null);
    if (!bytes) return fail(res, 413, "payload_too_large", "too large"), true;
    try {
      send(res, 200, importGoogleKeep(bytes, url.searchParams.get("dryRun") === "1"));
    } catch (e) {
      fail(res, e.status ?? 400, e.code ?? "bad_request", e.message);
    }
    return true;
  }
  if (method === "GET" && parts.length === 4 && parts[2] === "attachments") {
    const f = files.get(parts[3]);
    if (!f || !/^[0-9a-f]{64}\.[a-z0-9]{1,5}$/.test(parts[3])) return fail(res, 404, "not_found", "no such attachment"), true;
    res.writeHead(200, { "Content-Type": f.mimetype, "Content-Length": f.bytes.length, "Cache-Control": "private, max-age=31536000, immutable", ETag: `"${parts[3].slice(0, 16)}"` });
    res.end(f.bytes);
    return true;
  }

  const body = ["POST", "PATCH"].includes(method) ? await readJson(req, 800 * 1024).catch(() => ({})) : null;

  // The changes feed.
  if (parts.length === 3 && parts[2] === "changes" && method === "GET") {
    const rawSince = url.searchParams.get("since") ?? "";
    if (rawSince !== "" && !/^\d+$/.test(rawSince)) return fail(res, 400, "bad_request", "since: must be a number"), true;
    const since = Number(rawSince || 0);
    if (since > storeRev) return fail(res, 410, "cursor_expired", "the cursor is from another store"), true;
    const limit = Math.min(500, Math.max(1, Number(url.searchParams.get("limit") ?? 200) || 200));
    const horizon = Number(process.env.MOCK_KEEP_HORIZON ?? 0);
    if (since > 0 && since < horizon) return fail(res, 410, "cursor_expired", "the cursor is older than the kept tombstones"), true;
    const rows = [
      ...[...notes.values(), ...trash.values()].filter((n) => n.rev > since).map((n) => ({ rev: n.rev, note: n })),
      ...tombstones.filter((t) => t.rev > since).map((t) => ({ rev: t.rev, removed: t.id })),
    ].sort((a, b) => a.rev - b.rev);
    const page = rows.slice(0, limit);
    send(res, 200, {
      notes: page.filter((r) => r.note).map((r) => r.note),
      removed: page.filter((r) => r.removed).map((r) => r.removed),
      cursor: String(page.length ? page[page.length - 1].rev : storeRev),
      hasMore: rows.length > limit,
    });
    return true;
  }

  // Labels.
  if (parts[2] === "labels") {
    if (parts.length === 3 && method === "GET") return send(res, 200, { labels: allLabels() }), true;
    if (parts.length === 3 && method === "POST") {
      const name = cleanName(body?.name ?? "");
      const tag = normTag(name);
      if (!tag) return fail(res, 400, "bad_request", "name: must not be empty"), true;
      if (registry.has(tag)) return fail(res, 409, "label_exists", "that label exists"), true;
      registry.set(tag, { tag, name, createdAt: now() });
      send(res, 201, labelOut({ tag, name }));
      return true;
    }
    if (parts.length === 4) {
      const tag = decodeURIComponent(parts[3]);
      const l = registry.get(tag);
      if (!l && ![...notes.values()].some((n) => n.tags.includes(tag))) return fail(res, 404, "label_not_found", "no such label"), true;
      if (method === "PATCH") {
        const name = cleanName(body?.name ?? "");
        const next = normTag(name);
        if (!next) return fail(res, 400, "bad_request", "name: must not be empty"), true;
        if (next === tag) {
          registry.set(tag, { tag, name, createdAt: l?.createdAt ?? now() });
        } else {
          registry.delete(tag);
          if (!registry.has(next)) registry.set(next, { tag: next, name, createdAt: now() });
          for (const n of [...notes.values(), ...trash.values()]) {
            if (!n.tags.includes(tag)) continue;
            n.tags = [...new Set(n.tags.map((t) => (t === tag ? next : t)))];
            stamp(n);
          }
        }
        return send(res, 200, labelOut({ tag: next, name })), true;
      }
      if (method === "DELETE") {
        registry.delete(tag);
        let removed = 0;
        for (const n of [...notes.values(), ...trash.values()]) {
          if (!n.tags.includes(tag)) continue;
          n.tags = n.tags.filter((t) => t !== tag);
          stamp(n);
          removed++;
        }
        return send(res, 200, { ok: true, removed }), true;
      }
    }
    return fail(res, 404, "not_found", "Not found."), true;
  }

  // Batch writes (docs/keep.md): trash / restore / deleteForever exclude each other and set / labels.
  if (parts.length === 3 && parts[2] === "batch" && method === "POST") {
    const ids = [...new Set(Array.isArray(body?.ids) ? body.ids.map(String) : [])].slice(0, 500);
    const modes = ["trash", "restore", "deleteForever"].filter((k) => body?.[k]);
    if (modes.length > 1 || (modes.length && (body.set || body.addLabels || body.removeLabels))) {
      return fail(res, 400, "bad_request", "trash, restore and deleteForever exclude each other and set / labels"), true;
    }
    const changed = [];
    const removed = [];
    for (const key of ids) {
      const live = find(key);
      const n = live ?? trash.get(key);
      if (!n) continue;
      if (body.deleteForever) {
        if (!trash.has(n.id)) continue;
        purge(n);
        removed.push(n.id);
        continue;
      }
      if (body.restore) {
        if (!trash.has(n.id)) continue;
        restoreNote(n);
        changed.push(n);
        continue;
      }
      if (body.trash) {
        if (!notes.has(n.id)) continue;
        trashNote(n);
        changed.push(n);
        continue;
      }
      if (!notes.has(n.id)) continue;
      const before = JSON.stringify([n.color, n.pinned, n.archived, n.tags]);
      if (body.set) {
        if (body.set.color !== undefined && COLORS.includes(body.set.color)) n.color = body.set.color;
        if (body.set.pinned !== undefined) n.pinned = !!body.set.pinned;
        if (body.set.archived !== undefined) n.archived = !!body.set.archived;
      }
      if (body.addLabels || body.removeLabels) {
        const drop = new Set((body.removeLabels ?? []).map(normTag));
        n.tags = normTags([...n.tags, ...(body.addLabels ?? [])]).filter((t) => !drop.has(t));
      }
      if (JSON.stringify([n.color, n.pinned, n.archived, n.tags]) === before) continue;
      n.updatedAt = now();
      stamp(n);
      changed.push(n);
    }
    send(res, 200, { notes: changed, ...(body.deleteForever ? { removed } : {}) });
    return true;
  }

  if (parts.length === 2 && method === "GET") {
    const q = fold(url.searchParams.get("q") ?? "").trim();
    const tag = normTag(url.searchParams.get("tag") ?? "");
    const archived = url.searchParams.get("archived") ?? "include";
    const color = url.searchParams.get("color");
    const type = url.searchParams.get("type");
    const list = [...notes.values()]
      .filter((n) => !tag || n.tags.includes(tag))
      .filter((n) => archived === "include" || (archived === "only") === n.archived)
      .filter((n) => !color || n.color === color)
      .filter((n) => !type || (type === "list" ? !!n.items : type === "image" ? n.attachments.some((a) => a.mimetype.startsWith("image/")) : type === "reminder" ? !!n.reminder : true))
      .filter((n) => !q || fold(`${n.title} ${n.slug} ${n.tags.join(" ")} ${n.content}`).includes(q))
      .sort((a, b) => Number(b.pinned) - Number(a.pinned) || b.updatedAt.localeCompare(a.updatedAt));
    send(res, 200, { notes: list.map((n) => summary(n, q)) });
    return true;
  }
  if (parts.length === 2 && method === "POST") {
    const input = body ?? {};
    const title = String(input.title ?? "").trim();
    const content = String(input.content ?? "");
    if (Buffer.byteLength(content) > MAX_BYTES) return fail(res, 413, "note_too_large", "a note holds at most 256 KB of text"), true;
    if (!title && !content.trim() && !(Array.isArray(input.items) && input.items.length)) return fail(res, 400, "bad_request", "title: must not be empty"), true;
    let slug;
    if (input.slug) {
      slug = String(input.slug).toLowerCase();
      if (!validSlug(slug)) return fail(res, 400, "bad_request", "slug: lower-case letters, digits and single '-'"), true;
      if ([...notes.values(), ...trash.values()].some((n) => n.slug === slug)) return fail(res, 409, "slug_taken", `a note named "${slug}" exists`), true;
    } else {
      const first = title || content.split("\n").find((l) => l.trim()) || (input.items?.[0]?.text ?? "");
      slug = freeSlug(slugify(title || first.split(/\s+/).slice(0, 6).join(" ")) || "note");
    }
    let n;
    try {
      n = add({ slug, source: input.source || "web" }, now());
      n.position = typeof input.position === "number" ? input.position : Date.now();
      applyBody(n, { ...input, content: input.items ? undefined : content });
      n.updatedAt = n.createdAt;
      stamp(n);
      send(res, 201, n);
    } catch (e) {
      if (n) notes.delete(n.id);
      fail(res, e.status ?? 400, e.code ?? "bad_request", e.message);
    }
    return true;
  }
  if (parts.length === 3 && parts[2] === "trash" && method === "GET") {
    send(res, 200, { notes: [...trash.values()].sort((a, b) => b.deletedAt.localeCompare(a.deletedAt)).map((n) => summary(n)), days: TRASH_DAYS });
    return true;
  }
  if (parts.length === 3 && parts[2] === "trash" && method === "DELETE") {
    const all = [...trash.values()];
    for (const n of all) purge(n);
    send(res, 200, { ok: true, removed: all.length });
    return true;
  }
  if (parts.length === 4 && parts[2] === "trash" && method === "DELETE") {
    const n = trash.get(parts[3]);
    if (!n) return fail(res, 404, "note_not_found", "no such note"), true;
    purge(n);
    send(res, 200, { ok: true, removed: 1 });
    return true;
  }
  if (parts.length === 5 && parts[2] === "trash" && parts[4] === "restore" && method === "POST") {
    const n = trash.get(parts[3]);
    if (!n) return fail(res, 404, "note_not_found", "no such note"), true;
    restoreNote(n);
    send(res, 200, n);
    return true;
  }
  if (parts.length === 5 && parts[3] === "attachments" && method === "DELETE") {
    const n = find(parts[2]);
    if (!n) return fail(res, 404, "note_not_found", "no such note"), true;
    n.attachments = n.attachments.filter((a) => a.id !== parts[4]);
    n.updatedAt = now();
    stamp(n);
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
    try {
      applyBody(n, body ?? {});
    } catch (e) {
      return fail(res, e.status ?? 400, e.code ?? "bad_request", e.message), true;
    }
    n.updatedAt = now();
    stamp(n);
    send(res, 200, n);
    return true;
  }
  if (parts.length === 3 && method === "DELETE") {
    if (!n || trash.has(n.id)) return fail(res, 404, "note_not_found", "no such note"), true;
    trashNote(n);
    send(res, 200, { deleted: true, id: n.id, slug: n.slug, deletedAt: n.deletedAt });
    return true;
  }
  if (parts.length === 4 && parts[3] === "append" && method === "POST") {
    if (!n) return fail(res, 404, "note_not_found", "no such note"), true;
    const add2 = String(body?.content ?? "");
    const sep = n.content ? String(body?.separator ?? "") : "";
    if (Buffer.byteLength(n.content + sep + add2) > MAX_BYTES) return fail(res, 413, "note_too_large", "too large"), true;
    n.content += sep + add2;
    if (n.items) {
      n.items = contentToItems(n.content);
      n.content = itemsToContent(n.items);
    }
    n.updatedAt = now();
    stamp(n);
    send(res, 200, n);
    return true;
  }
  fail(res, 404, "not_found", "Not found.");
  return true;
}
