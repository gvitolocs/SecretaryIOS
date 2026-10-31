// Notebooks for the mock server (docs/design/notebooks-api.md): /v1/notebooks, in memory.
// Seeds: a "Research Methods" notebook with a PDF, a web page, a YouTube video, pasted text, a source
// still being read and one that failed; linked notes; a chat with a cited answer; insights; podcasts.
// New sources, podcasts and insights move through their states by the clock (a few seconds each), so
// the page's polling can be watched. A chat reply streams word by word (text/event-stream).
// MOCK_NOTEBOOKS=empty starts with no notebooks.

import { randomBytes } from "node:crypto";

const rid = (p) => `${p}${randomBytes(5).toString("hex")}`;
const iso = (ms = Date.now()) => new Date(ms).toISOString();
const MIN = 60_000;
const DAY = 24 * 60 * MIN;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const slugOf = (s) =>
  String(s).normalize("NFKD").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 64) || "note";

/** @type {Map<string, any>} */ const notebooks = new Map();
/** @type {Map<string, any>} */ const sources = new Map(); // id -> { ...Source, passages: string[], pageOf?: number[] }
/** @type {Map<string, Set<string>>} */ const membership = new Map(); // notebook id -> source ids
/** @type {Map<string, Map<string, string>>} */ const contexts = new Map(); // notebook id -> source id -> context
/** @type {Map<string, any[]>} */ const noteLinks = new Map();
/** @type {Map<string, any>} */ const chats = new Map();
/** @type {Map<string, any[]>} */ const insights = new Map(); // source id -> insights
/** @type {Map<string, any>} */ const podcasts = new Map();

const TRANSFORMATIONS = [
  { id: "t-dense-summary", name: "Dense Summary", description: "A compact summary that keeps the details.", prompt: "Summarise densely.", apply_default: true, builtin: true },
  { id: "t-key-insights", name: "Key Insights", description: "The points worth remembering.", prompt: "List the key insights.", apply_default: false, builtin: true },
  { id: "t-reflections", name: "Reflection Questions", description: "Questions to test your understanding.", prompt: "Ask reflection questions.", apply_default: false, builtin: true },
  { id: "t-table-of-contents", name: "Table of Contents", description: "The structure of the source.", prompt: "Outline the source.", apply_default: false, builtin: true },
];

const PROFILES = {
  episode_profiles: [
    { id: "two-hosts", name: "Two hosts", description: "A relaxed conversation between two hosts." },
    { id: "deep-dive", name: "Deep dive", description: "One host walks through the material in depth." },
    { id: "exam-prep", name: "Exam prep", description: "Questions and answers to revise from." },
  ],
  speaker_profiles: [
    { id: "lucina-duo", name: "Lucina and Marco", description: "Lucina with a male co-host." },
    { id: "lucina-solo", name: "Lucina solo", description: "Lucina alone." },
  ],
};

const PASSAGES = {
  rm: [
    "Research methods courses separate three questions: what you want to know, how you will find out, and how you will know that the answer is any good.",
    "Reliability is about consistency: a measurement is reliable when repeating it under the same conditions gives the same result. A bathroom scale that shows 71.2 kg three mornings in a row is reliable.",
    "Validity is about meaning: a measurement is valid when it measures what it claims to measure. The same scale is not a valid measure of fitness, however reliable it is.",
    "Reliability is a necessary but not a sufficient condition for validity. An instrument can be reliable without being valid, but it cannot be valid if it is not reliable.",
    "Sampling decides who the findings are about. A probability sample gives every member of the population a known chance of being picked; a convenience sample gives no such guarantee.",
    "Qualitative designs trade breadth for depth. Semi-structured interviews keep a list of topics but let the participant lead, and the analysis looks for patterns of meaning rather than counts.",
    "Triangulation combines methods, sources or researchers to check a finding from more than one side. Agreement between independent approaches raises confidence; disagreement is a result too.",
    "Ethics is part of the design, not an afterthought: informed consent, the right to withdraw, privacy of the data and honest reporting of what was and was not found.",
  ],
  web: [
    "A semi-structured interview follows an interview guide of open questions. The interviewer may reorder them, skip some and ask follow-ups that the guide did not foresee.",
    "Good interview questions are open, neutral and about experience: \"Can you tell me about the last time you used the library?\" rather than \"Don't you find the library too noisy?\"",
    "Recording and transcribing interviews lets the analysis stay close to the participants' own words. Field notes capture what a recording cannot: setting, mood, interruptions.",
    "Thematic analysis moves from codes to themes: read the transcripts, mark meaningful segments, group the codes and name the themes, then check them against the data again.",
  ],
  yt: [
    "Start with the decision the survey has to support. If no answer would change what you do, the question does not belong in the survey.",
    "Closed questions are easy to analyse and hard to write well. Offer response options that are exhaustive and do not overlap, and allow \"don't know\" when it is a real answer.",
    "Pilot the survey on five people before you send it to five hundred. Watch where they hesitate: that is where the question is unclear.",
  ],
  text: [
    "Reading list: Bryman, Social Research Methods, chapters 8 and 12. Robson, Real World Research, chapter 6. Article on measurement validity from the course page.",
    "Exam format: two hours, four questions out of six. One must be a design question where you justify the method you pick.",
  ],
};

function mkSource(spec) {
  const created = spec.created ?? Date.now() - DAY;
  const passages = spec.passages ?? [];
  const src = {
    id: spec.id ?? rid("s"),
    kind: spec.kind,
    title: spec.title,
    origin: spec.origin,
    // A source that is "being read" moves on by the clock from `since`.
    since: spec.since ?? null,
    finalStatus: spec.status ?? "ready",
    error: spec.error ?? null,
    chars: passages.reduce((n, p) => n + p.length, 0),
    pages: spec.pages ?? null,
    duration_s: spec.duration_s ?? null,
    has_file: !!spec.has_file,
    created_at: iso(created),
    updated_at: iso(created),
    passages,
    pageOf: spec.pageOf ?? null,
    notebooks: [],
  };
  sources.set(src.id, src);
  return src;
}

/** The source as the API shows it: status by the clock for sources that are still being read. */
function view(src, nbId) {
  let status = src.finalStatus;
  if (src.since !== null && status === "ready") {
    const age = Date.now() - src.since;
    status = age < 2500 ? "queued" : age < 6000 ? "extracting" : age < 9000 ? "embedding" : "ready";
  }
  const ready = status === "ready";
  const out = {
    id: src.id,
    kind: src.kind,
    title: src.title,
    origin: src.origin,
    status,
    error: status === "failed" ? src.error : null,
    chars: ready ? src.chars : 0,
    chunks: ready ? src.passages.length : 0,
    pages: ready ? src.pages : null,
    duration_s: src.duration_s,
    has_file: src.has_file,
    created_at: src.created_at,
    updated_at: src.updated_at,
    notebooks: [...membership.entries()].filter(([, set]) => set.has(src.id)).map(([id]) => id),
  };
  if (nbId) out.context = contexts.get(nbId)?.get(src.id) ?? "full";
  return out;
}

function counts(id) {
  return {
    sources: membership.get(id)?.size ?? 0,
    notes: noteLinks.get(id)?.length ?? 0,
    chats: [...chats.values()].filter((c) => c.notebook_id === id).length,
    podcasts: [...podcasts.values()].filter((p) => p.notebook_id === id).length,
  };
}

const notebookView = (nb) => ({
  id: nb.id,
  name: nb.name,
  description: nb.description,
  archived: nb.archived,
  created_at: nb.created_at,
  updated_at: nb.updated_at,
  counts: counts(nb.id),
});

function addTo(nbId, src) {
  membership.get(nbId).add(src.id);
  contexts.get(nbId).set(src.id, "full");
  touch(nbId);
}

function touch(nbId) {
  const nb = notebooks.get(nbId);
  if (nb) nb.updated_at = iso();
}

function newNotebook(name, description = "", ago = 0) {
  const nb = { id: rid("nb"), name, description, archived: false, created_at: iso(Date.now() - ago - DAY), updated_at: iso(Date.now() - ago) };
  notebooks.set(nb.id, nb);
  membership.set(nb.id, new Set());
  contexts.set(nb.id, new Map());
  noteLinks.set(nb.id, []);
  return nb;
}

const PODCAST_STEPS = ["queued", "outline", "script", "voicing", "mixing", "ready"];
const STEP_MS = 2500;

function podcastView(p) {
  let status = p.finalStatus;
  if (p.since !== null && status === "ready") {
    const i = Math.floor((Date.now() - p.since) / STEP_MS);
    status = PODCAST_STEPS[Math.min(i, PODCAST_STEPS.length - 1)];
  }
  const ready = status === "ready";
  return {
    id: p.id,
    notebook_id: p.notebook_id,
    title: p.title,
    episode_profile: p.episode_profile,
    speaker_profile: p.speaker_profile,
    briefing: p.briefing,
    status,
    error: status === "failed" ? p.error : null,
    outline: status === "queued" || status === "outline" ? [] : p.outline,
    transcript: ready ? p.transcript : [],
    duration_s: ready ? p.duration_s : null,
    audio: ready,
    created_at: p.created_at,
  };
}

const insightView = (i) => {
  const age = Date.now() - i.since;
  const status = i.finalStatus === "ready" && i.since ? (age < 2000 ? "queued" : age < 4500 ? "running" : "ready") : i.finalStatus;
  return { id: i.id, source_id: i.source_id, transformation_id: i.transformation_id, title: i.title, status, text: status === "ready" ? i.text : "", created_at: i.created_at };
};

function seed() {
  if (process.env.MOCK_NOTEBOOKS === "empty") return;
  const rm = newNotebook("Research Methods", "Lecture notes and readings for the exam", 3 * 60 * MIN);
  const pdf = mkSource({ kind: "pdf", title: "RM Class 2.pdf", origin: "upload:RM Class 2.pdf", passages: PASSAGES.rm, pages: 22, has_file: true, pageOf: [2, 3, 4, 5, 8, 11, 15, 19] });
  const web = mkSource({ kind: "web", title: "Semi-structured interviews, a practical guide", origin: "https://www.qualitative-methods.example/interviews/semi-structured", passages: PASSAGES.web });
  const yt = mkSource({ kind: "youtube", title: "How to design a survey that works", origin: "https://www.youtube.com/watch?v=AbC123xyz", passages: PASSAGES.yt, duration_s: 1124 });
  const txt = mkSource({ kind: "text", title: "Reading list and exam format", origin: "text", passages: PASSAGES.text });
  const busy = mkSource({ kind: "web", title: "Sampling methods overview", origin: "https://stats.example/sampling-overview", passages: ["Sampling methods overview."], status: "embedding" });
  const broken = mkSource({ kind: "pdf", title: "Scanned handout.pdf", origin: "upload:Scanned handout.pdf", status: "failed", error: "No text could be extracted: the PDF looks like a scan. Upload a version with selectable text.", has_file: true });
  for (const s of [pdf, web, yt, txt, busy, broken]) addTo(rm.id, s);
  contexts.get(rm.id).set(txt.id, "summary");
  contexts.get(rm.id).set(broken.id, "off");
  noteLinks.get(rm.id).push(
    { slug: "craftit-meeting", title: "CRAFT-IT4SD meeting", excerpt: "Briefing for the meeting with Marianne about paid hours and flexible times.", updated_at: iso(Date.now() - 2 * DAY), indexed: true },
    { slug: "rm-class-2-summary", title: "RM Class 2 summary", excerpt: "Reliability vs validity, the scale example, why sampling decides who the findings are about.", updated_at: iso(Date.now() - 40 * MIN), indexed: true },
  );
  insights.set(pdf.id, [
    {
      id: rid("i"),
      source_id: pdf.id,
      transformation_id: "t-dense-summary",
      title: "Dense Summary",
      finalStatus: "ready",
      since: 0,
      text: "The class separates **reliability** (consistent results) from **validity** (measuring what you claim to). Reliability is necessary but not sufficient for validity [source:" + pdf.id + "-3]. Sampling decides who the findings are about, and triangulation checks a finding from several sides.",
      created_at: iso(Date.now() - 50 * MIN),
    },
  ]);
  const nbChat = { id: rid("c"), notebook_id: rm.id, title: "Exam prep", created_at: iso(Date.now() - 2 * 60 * MIN), updated_at: iso(Date.now() - 20 * MIN), messages: [] };
  const first = { id: rid("m"), role: "user", text: "What is the difference between reliability and validity?", created_at: iso(Date.now() - 21 * MIN) };
  const answerText = `Reliability is about **consistency**: a measurement is reliable when repeating it under the same conditions gives the same result [source:${pdf.id}-1]. Validity is about **meaning**: it asks whether the measurement captures what it claims to measure [source:${pdf.id}-2].\n\nThe two are linked in one direction only. An instrument can be reliable without being valid, but it cannot be valid if it is not reliable [source:${pdf.id}-3]. Your own summary makes the same point [note:rm-class-2-summary].`;
  const reply = { id: rid("m"), role: "assistant", text: answerText, citations: citationsFor(answerText), model: "deepseek-flash", created_at: iso(Date.now() - 20 * MIN) };
  nbChat.messages.push(first, reply);
  chats.set(nbChat.id, nbChat);
  const other = { id: rid("c"), notebook_id: rm.id, title: "Interview questions", created_at: iso(Date.now() - DAY), updated_at: iso(Date.now() - DAY), messages: [] };
  chats.set(other.id, other);
  const pod = {
    id: rid("p"),
    notebook_id: rm.id,
    title: "Reliability, validity and sampling",
    episode_profile: "two-hosts",
    speaker_profile: "lucina-duo",
    briefing: "Focus on the exam: what to remember and the traps.",
    finalStatus: "ready",
    since: null,
    error: null,
    outline: [
      { name: "Reliability vs validity", description: "The scale example and why one does not imply the other.", size: "medium" },
      { name: "Sampling", description: "Who the findings are about.", size: "short" },
    ],
    transcript: [
      { speaker: "Lucina", text: "Today: the two words every methods exam asks about, reliability and validity." },
      { speaker: "Marco", text: "Reliable means you get the same answer each time. Like a scale that always says 71.2." },
      { speaker: "Lucina", text: "And valid means it measures the right thing. That scale tells you nothing about fitness." },
    ],
    duration_s: 312.4,
    created_at: iso(Date.now() - 3 * 60 * MIN),
  };
  podcasts.set(pod.id, pod);

  const aarhus = newNotebook("Thesis reading", "Papers for the literature review", 2 * DAY);
  addTo(aarhus.id, web);
  const thesis = mkSource({ kind: "pdf", title: "Mixed methods research: a guide.pdf", origin: "upload:Mixed methods research.pdf", passages: PASSAGES.rm.slice(4), pages: 14, has_file: true });
  addTo(aarhus.id, thesis);
  newNotebook("Trip to Japan", "Itineraries, trains and restaurants", 6 * DAY);
  const old = newNotebook("Italian cooking", "Recipes from the family cookbook", 40 * DAY);
  old.archived = true;
}

/** The resolved citation objects of the [source:…] / [insight:…] / [note:…] refs in a text. */
function citationsFor(text) {
  const out = [];
  for (const m of text.matchAll(/\[(source|insight|note):([A-Za-z0-9_-]+)\]/g)) {
    const ref = `${m[1]}:${m[2]}`;
    if (out.some((c) => c.ref === ref)) continue;
    if (m[1] === "source") {
      const dash = m[2].lastIndexOf("-");
      const src = sources.get(m[2].slice(0, dash));
      const chunk = Number(m[2].slice(dash + 1));
      if (!src || !src.passages[chunk]) continue;
      out.push({ ref, kind: "source", source_id: src.id, chunk, title: src.title, excerpt: src.passages[chunk].slice(0, 160), page: src.pageOf?.[chunk] ?? null, start_s: null });
    } else if (m[1] === "note") {
      out.push({ ref, kind: "note", source_id: null, chunk: null, title: m[2], excerpt: "", page: null, start_s: null });
    }
  }
  return out;
}

function answerFor(nbId, question) {
  const ids = [...(membership.get(nbId) ?? [])].map((id) => sources.get(id)).filter((s) => s && view(s).status === "ready" && (contexts.get(nbId)?.get(s.id) ?? "full") !== "off");
  if (!ids.length) return "I have no readable sources in this notebook yet, so I can't answer from them. Add a source and ask again.";
  const words = new Set(question.toLowerCase().match(/[a-zà-ü]{4,}/g) ?? []);
  let best = [];
  for (const s of ids) {
    s.passages.forEach((p, i) => {
      const score = (p.toLowerCase().match(/[a-zà-ü]{4,}/g) ?? []).filter((w) => words.has(w)).length;
      best.push({ s, i, score });
    });
  }
  best.sort((a, b) => b.score - a.score);
  const top = best.slice(0, 2);
  const lead = top[0];
  const second = top[1];
  return `From your sources: ${lead.s.passages[lead.i].split(". ")[0].replace(/\.$/, "")} [source:${lead.s.id}-${lead.i}].${second ? `\n\nAlso relevant: ${second.s.passages[second.i].split(". ")[0].replace(/\.$/, "")} [source:${second.s.id}-${second.i}].` : ""}`;
}

/** A 2 s silent mono WAV, so the audio player has something to play in the mock. */
function silentWav() {
  const rate = 8000;
  const n = rate * 2;
  const buf = Buffer.alloc(44 + n);
  buf.write("RIFF", 0);
  buf.writeUInt32LE(36 + n, 4);
  buf.write("WAVEfmt ", 8);
  buf.writeUInt32LE(16, 16);
  buf.writeUInt16LE(1, 20);
  buf.writeUInt16LE(1, 22);
  buf.writeUInt32LE(rate, 24);
  buf.writeUInt32LE(rate, 28);
  buf.writeUInt16LE(1, 32);
  buf.writeUInt16LE(8, 34);
  buf.write("data", 36);
  buf.writeUInt32LE(n, 40);
  buf.fill(128, 44);
  return buf;
}

function readRaw(req, limit = 220 * 1024 * 1024) {
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

let seeded = false;

export async function handleNotebooks(req, res, { path, method, url, send, fail, readJson }) {
  if (path !== "/v1/notebooks" && !path.startsWith("/v1/notebooks/")) return false;
  if (!seeded) {
    seeded = true;
    seed();
  }
  const rest = path.slice("/v1/notebooks".length).replace(/^\//, "");
  const seg = rest ? rest.split("/").map(decodeURIComponent) : [];
  const q = url.searchParams;
  const nbOr404 = (id) => notebooks.get(id) ?? null;
  const notFound = () => fail(res, 404, "not_found", "Not found.");
  const body = async () => {
    try {
      return await readJson(req);
    } catch (e) {
      fail(res, e.status ?? 400, "bad_request", e.message);
      return null;
    }
  };

  // ---- notebooks
  if (!seg.length) {
    if (method === "GET") {
      const filter = q.get("archived") ?? "exclude";
      const list = [...notebooks.values()]
        .filter((n) => (filter === "include" ? true : filter === "only" ? n.archived : !n.archived))
        .sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at))
        .map(notebookView);
      return send(res, 200, { notebooks: list }), true;
    }
    if (method === "POST") {
      const b = await body();
      if (!b) return true;
      if (typeof b.name !== "string" || !b.name.trim()) return fail(res, 400, "bad_request", "name: required"), true;
      const nb = newNotebook(b.name.trim(), typeof b.description === "string" ? b.description.trim() : "");
      return send(res, 201, notebookView(nb)), true;
    }
    return fail(res, 405, "bad_request", "Method not allowed."), true;
  }

  const head = seg[0];

  if (head === "health") return send(res, 200, { ok: true, sources: sources.size, chunks: [...sources.values()].reduce((n, s) => n + s.passages.length, 0), embedder: "mock" }), true;

  if (head === "transformations") {
    if (method === "GET" && seg.length === 1) return send(res, 200, { transformations: TRANSFORMATIONS }), true;
    return fail(res, 405, "bad_request", "The mock only lists transformations."), true;
  }

  if (head === "podcast-profiles") return send(res, 200, PROFILES), true;

  if (head === "search" && method === "GET") {
    return send(res, 200, { hits: searchAll(null, q.get("q") ?? "", Number(q.get("limit")) || 20) }), true;
  }

  // ---- sources by id
  if (head === "sources") {
    const src = sources.get(seg[1]);
    if (!src) return notFound(), true;
    const action = seg[2];
    if (!action && method === "GET") return send(res, 200, { ...view(src), ...(q.get("text") ? { text: src.passages.join("\n\n") } : {}) }), true;
    if (action === "chunks" && method === "GET") {
      const around = q.get("around");
      let from = Number(q.get("from")) || 0;
      let limit = Number(q.get("limit")) || 40;
      if (around !== null) {
        from = Math.max(0, Number(around) - 3);
        limit = 8;
      }
      const list = src.passages.slice(from, from + limit).map((text, k) => ({ id: `${src.id}-${from + k}`, ordinal: from + k, text, ...(src.pageOf?.[from + k] ? { page: src.pageOf[from + k] } : {}) }));
      return send(res, 200, { chunks: list }), true;
    }
    if (action === "file" && method === "GET") {
      res.writeHead(200, { "Content-Type": "text/plain; charset=utf-8" });
      return res.end(src.passages.join("\n\n")), true;
    }
    if (action === "retry" && method === "POST") {
      src.finalStatus = "ready";
      src.error = null;
      src.since = Date.now();
      if (!src.passages.length) src.passages = ["Retried: the text of this source."];
      src.chars = src.passages.join("").length;
      return send(res, 202, view(src)), true;
    }
    if (action === "insights") {
      if (method === "GET") return send(res, 200, { insights: (insights.get(src.id) ?? []).map(insightView) }), true;
      if (method === "POST") {
        const b = await body();
        if (!b) return true;
        const tr = TRANSFORMATIONS.find((t) => t.id === b.transformation_id);
        if (!tr) return fail(res, 404, "not_found", "No such transformation."), true;
        const i = {
          id: rid("i"),
          source_id: src.id,
          transformation_id: tr.id,
          title: tr.name,
          finalStatus: "ready",
          since: Date.now(),
          text: `**${tr.name}** of "${src.title}"\n\n- ${src.passages.slice(0, 3).map((p) => p.split(". ")[0]).join("\n- ")}`,
          created_at: iso(),
        };
        insights.set(src.id, [i, ...(insights.get(src.id) ?? [])]);
        return send(res, 202, insightView(i)), true;
      }
    }
    return notFound(), true;
  }

  if (head === "insights") {
    const all = [...insights.values()].flat();
    const ins = all.find((i) => i.id === seg[1]);
    if (!ins) return notFound(), true;
    if (seg[2] === "note" && method === "POST") {
      const slug = slugOf(`${ins.title} ${sources.get(ins.source_id)?.title ?? ""}`);
      const link = { slug, title: `${ins.title}: ${sources.get(ins.source_id)?.title ?? ""}`, excerpt: ins.text.replace(/[*#-]/g, "").slice(0, 140), updated_at: iso(), indexed: true };
      for (const [nbId, set] of membership) if (set.has(ins.source_id)) noteLinks.get(nbId).push(link);
      return send(res, 201, link), true;
    }
    if (!seg[2] && method === "DELETE") {
      for (const [k, list] of insights) insights.set(k, list.filter((i) => i.id !== ins.id));
      return send(res, 200, { deleted: true }), true;
    }
    return notFound(), true;
  }

  // ---- chats by id
  if (head === "chats") {
    const chat = chats.get(seg[1]);
    if (!chat) return notFound(), true;
    if (!seg[2]) {
      if (method === "GET") return send(res, 200, { id: chat.id, notebook_id: chat.notebook_id, title: chat.title, created_at: chat.created_at, updated_at: chat.updated_at, messages: chat.messages }), true;
      if (method === "PATCH") {
        const b = await body();
        if (!b) return true;
        if (typeof b.title === "string" && b.title.trim()) chat.title = b.title.trim();
        return send(res, 200, chatView(chat)), true;
      }
      if (method === "DELETE") {
        chats.delete(chat.id);
        return send(res, 200, { deleted: true }), true;
      }
    }
    if (seg[2] === "messages" && method === "POST" && seg.length === 3) {
      const b = await body();
      if (!b) return true;
      if (typeof b.text !== "string" || !b.text.trim()) return fail(res, 400, "bad_request", "text: required"), true;
      const user = { id: rid("m"), role: "user", text: b.text.trim(), created_at: iso() };
      const text = answerFor(chat.notebook_id, user.text);
      const reply = { id: rid("m"), role: "assistant", text, citations: citationsFor(text), model: b.model === "qwen" ? "qwen-local" : "deepseek-flash", created_at: iso() };
      chat.messages.push(user, reply);
      chat.updated_at = iso();
      if (String(req.headers.accept ?? "").includes("text/event-stream") && process.env.MOCK_NOTEBOOKS_STREAM !== "off") {
        res.writeHead(200, { "Content-Type": "text/event-stream; charset=utf-8", "Cache-Control": "no-store", Connection: "keep-alive" });
        await sleep(400);
        for (const piece of text.match(/\S+\s*/g) ?? []) {
          res.write(`event: delta\ndata: ${JSON.stringify({ text: piece })}\n\n`);
          await sleep(45);
        }
        res.write(`event: message\ndata: ${JSON.stringify({ user, reply })}\n\n`);
        return res.end(), true;
      }
      await sleep(600);
      return send(res, 200, { user, reply }), true;
    }
    if (seg[2] === "messages" && seg[4] === "note" && method === "POST") {
      const m = chat.messages.find((x) => x.id === seg[3]);
      if (!m) return notFound(), true;
      const b = (await body()) ?? {};
      const title = (typeof b.title === "string" && b.title.trim()) || `Answer: ${chat.title}`;
      const link = { slug: slugOf(title), title, excerpt: m.text.replace(/\[[a-z]+:[^\]]+\]/g, "").slice(0, 140), updated_at: iso(), indexed: true };
      noteLinks.get(chat.notebook_id).push(link);
      touch(chat.notebook_id);
      return send(res, 201, link), true;
    }
    return notFound(), true;
  }

  // ---- podcasts by id
  if (head === "podcasts") {
    const p = podcasts.get(seg[1]);
    if (!p) return notFound(), true;
    if (!seg[2] && method === "GET") return send(res, 200, podcastView(p)), true;
    if (!seg[2] && method === "DELETE") {
      podcasts.delete(p.id);
      return send(res, 200, { deleted: true }), true;
    }
    if (seg[2] === "audio" && method === "GET") {
      const wav = silentWav();
      res.writeHead(200, { "Content-Type": "audio/wav", "Content-Length": wav.length, "Accept-Ranges": "bytes" });
      return res.end(wav), true;
    }
    return notFound(), true;
  }

  // ---- one notebook
  const nb = nbOr404(head);
  if (!nb) return notFound(), true;
  const sub = seg[1];

  if (!sub) {
    if (method === "GET") {
      return (
        send(res, 200, {
          ...notebookView(nb),
          sources: [...membership.get(nb.id)].map((id) => view(sources.get(id), nb.id)),
          notes: noteLinks.get(nb.id),
          chats: [...chats.values()].filter((c) => c.notebook_id === nb.id).map(chatView),
          podcasts: [...podcasts.values()].filter((p) => p.notebook_id === nb.id).map(podcastView),
        }),
        true
      );
    }
    if (method === "PATCH") {
      const b = await body();
      if (!b) return true;
      if (typeof b.name === "string" && b.name.trim()) nb.name = b.name.trim();
      if (typeof b.description === "string") nb.description = b.description.trim();
      if (typeof b.archived === "boolean") nb.archived = b.archived;
      touch(nb.id);
      return send(res, 200, notebookView(nb)), true;
    }
    if (method === "DELETE") {
      let deleted = 0;
      if (q.get("sources") === "delete-exclusive") {
        for (const id of membership.get(nb.id)) {
          const elsewhere = [...membership.entries()].some(([other, set]) => other !== nb.id && set.has(id));
          if (!elsewhere) {
            sources.delete(id);
            deleted++;
          }
        }
      }
      for (const [id, c] of chats) if (c.notebook_id === nb.id) chats.delete(id);
      for (const [id, p] of podcasts) if (p.notebook_id === nb.id) podcasts.delete(id);
      notebooks.delete(nb.id);
      membership.delete(nb.id);
      contexts.delete(nb.id);
      noteLinks.delete(nb.id);
      return send(res, 200, { deleted: true, sources_deleted: deleted }), true;
    }
    return notFound(), true;
  }

  if (sub === "sources") {
    if (seg.length === 2 && method === "POST") {
      const b = await body();
      if (!b) return true;
      let src;
      if (b.kind === "url") {
        if (!/^https?:\/\//i.test(String(b.url))) return fail(res, 400, "bad_request", "url: must start with http:// or https://"), true;
        const yt = /youtu\.?be/.test(b.url);
        src = mkSource({ kind: yt ? "youtube" : "web", title: yt ? "YouTube video" : new URL(b.url).hostname, origin: b.url, passages: PASSAGES.web, since: Date.now(), ...(yt ? { duration_s: 600 } : {}) });
      } else if (b.kind === "text") {
        if (typeof b.text !== "string" || !b.text.trim()) return fail(res, 400, "bad_request", "text: required"), true;
        src = mkSource({ kind: "text", title: b.title?.trim() || b.text.trim().slice(0, 40), origin: "text", passages: b.text.split(/\n{2,}/).filter(Boolean), since: Date.now() });
      } else if (b.kind === "sandbox") {
        src = mkSource({ kind: "file", title: String(b.path).split("/").pop(), origin: `sandbox:${b.path}`, passages: PASSAGES.rm.slice(0, 3), since: Date.now() });
      } else if (b.kind === "note") {
        src = mkSource({ kind: "note", title: String(b.slug), origin: `note:${b.slug}`, passages: ["The text of the note."], since: Date.now() });
      } else if (b.kind === "source") {
        src = sources.get(b.id);
        if (!src) return notFound(), true;
      } else return fail(res, 400, "bad_request", "kind: url, text, sandbox, note or source"), true;
      addTo(nb.id, src);
      return send(res, 202, view(src, nb.id)), true;
    }
    if (seg[2] === "upload" && method === "POST") {
      const raw = await readRaw(req).catch(() => null);
      if (!raw) return fail(res, 413, "bad_request", "File too large."), true;
      const name = /filename="([^"]+)"/.exec(raw.subarray(0, 2048).toString("latin1"))?.[1] ?? "upload";
      const pdf = /\.pdf$/i.test(name);
      const src = mkSource({ kind: pdf ? "pdf" : /\.(mp3|m4a|wav)$/i.test(name) ? "audio" : "file", title: name, origin: `upload:${name}`, passages: PASSAGES.rm.slice(0, 4), since: Date.now(), has_file: true, ...(pdf ? { pages: 8 } : {}) });
      addTo(nb.id, src);
      return send(res, 202, view(src, nb.id)), true;
    }
    const src = sources.get(seg[2]);
    if (seg.length === 3 && src && membership.get(nb.id).has(src.id)) {
      if (method === "PATCH") {
        const b = await body();
        if (!b) return true;
        if (!["full", "summary", "off"].includes(b.context)) return fail(res, 400, "bad_request", "context: full, summary or off"), true;
        contexts.get(nb.id).set(src.id, b.context);
        return send(res, 200, view(src, nb.id)), true;
      }
      if (method === "DELETE") {
        membership.get(nb.id).delete(src.id);
        const elsewhere = [...membership.values()].some((set) => set.has(src.id));
        if (!elsewhere) sources.delete(src.id);
        touch(nb.id);
        return send(res, 200, { removed: true, deleted: !elsewhere }), true;
      }
    }
    return notFound(), true;
  }

  if (sub === "notes") {
    if (seg.length === 2 && method === "POST") {
      const b = await body();
      if (!b) return true;
      let link;
      if (typeof b.slug === "string") {
        link = { slug: b.slug, title: b.slug.replace(/-/g, " ").replace(/^./, (c) => c.toUpperCase()), excerpt: "", updated_at: iso(), indexed: true };
      } else if (typeof b.title === "string" && typeof b.text === "string") {
        link = { slug: slugOf(b.title), title: b.title.trim(), excerpt: b.text.trim().slice(0, 140), updated_at: iso(), indexed: true };
      } else return fail(res, 400, "bad_request", "slug, or title and text"), true;
      const list = noteLinks.get(nb.id);
      if (!list.some((n) => n.slug === link.slug)) list.unshift(link);
      touch(nb.id);
      return send(res, 201, link), true;
    }
    if (seg.length === 3 && method === "DELETE") {
      noteLinks.set(nb.id, noteLinks.get(nb.id).filter((n) => n.slug !== seg[2]));
      touch(nb.id);
      return send(res, 200, { removed: true }), true;
    }
    return notFound(), true;
  }

  if (sub === "search" && method === "GET") return send(res, 200, { hits: searchAll(nb.id, q.get("q") ?? "", Number(q.get("limit")) || 20) }), true;

  if (sub === "chats") {
    if (method === "GET") return send(res, 200, { chats: [...chats.values()].filter((c) => c.notebook_id === nb.id).map(chatView) }), true;
    if (method === "POST") {
      const b = (await body()) ?? {};
      const chat = { id: rid("c"), notebook_id: nb.id, title: (typeof b.title === "string" && b.title.trim()) || "New chat", created_at: iso(), updated_at: iso(), messages: [] };
      chats.set(chat.id, chat);
      return send(res, 201, chatView(chat)), true;
    }
  }

  if (sub === "podcasts") {
    if (method === "GET") return send(res, 200, { podcasts: [...podcasts.values()].filter((p) => p.notebook_id === nb.id).map(podcastView) }), true;
    if (method === "POST") {
      const b = (await body()) ?? {};
      const p = {
        id: rid("p"),
        notebook_id: nb.id,
        title: `${nb.name}: episode ${[...podcasts.values()].filter((x) => x.notebook_id === nb.id).length + 1}`,
        episode_profile: b.episode_profile ?? "two-hosts",
        speaker_profile: b.speaker_profile ?? "lucina-duo",
        briefing: b.briefing ?? "",
        finalStatus: "ready",
        since: Date.now(),
        error: null,
        outline: [{ name: "Introduction", description: "What the notebook is about.", size: "short" }],
        transcript: [
          { speaker: "Lucina", text: `Welcome. Today we go through ${nb.name}.` },
          { speaker: "Marco", text: "Let's start with the main ideas from your sources." },
        ],
        duration_s: (Number(b.minutes) || 8) * 60,
        created_at: iso(),
      };
      podcasts.set(p.id, p);
      touch(nb.id);
      return send(res, 202, podcastView(p)), true;
    }
  }

  return notFound(), true;
}

function chatView(c) {
  return { id: c.id, notebook_id: c.notebook_id, title: c.title, created_at: c.created_at, updated_at: c.updated_at, messages: c.messages.length };
}

function searchAll(nbId, query, limit) {
  const words = query.toLowerCase().match(/[a-zà-ü0-9]{2,}/g) ?? [];
  if (!words.length) return [];
  const hits = [];
  const ids = nbId ? [nbId] : [...notebooks.keys()];
  for (const id of ids) {
    for (const sid of membership.get(id) ?? []) {
      const src = sources.get(sid);
      src?.passages.forEach((p, i) => {
        const score = words.filter((w) => p.toLowerCase().includes(w)).length / words.length;
        if (score > 0) hits.push({ ref: `source:${sid}-${i}`, kind: "source", source_id: sid, title: src.title, excerpt: p.slice(0, 160), score, ...(nbId ? {} : { notebook_id: id }) });
      });
    }
    for (const n of noteLinks.get(id) ?? []) {
      const hay = `${n.title} ${n.excerpt}`.toLowerCase();
      const score = words.filter((w) => hay.includes(w)).length / words.length;
      if (score > 0) hits.push({ ref: `note:${n.slug}`, kind: "note", slug: n.slug, title: n.title, excerpt: n.excerpt, score, ...(nbId ? {} : { notebook_id: id }) });
    }
  }
  return hits.sort((a, b) => b.score - a.score).slice(0, limit);
}
