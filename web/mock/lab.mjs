// The AI Lab for the dev mock (docs/media-library.md, "AI Lab"): the same routes as lucina-media's `lab_http.rs`, with a
// pretend model. Jobs run one at a time: queued, then 2 s waiting for the GPU, then progress over MOCK_LAB_MS (default
// 12000 ms), then done. A prompt containing "fail" ends in a failed job. Accept adds a new item (source `lucina-lab`)
// next to the original; the pictures are the Gallery's synthetic SVGs, the result in shifted colours.

const RUN_MS = Number(process.env.MOCK_LAB_MS) || 12_000;
const WAIT_MS = 2_000;
const jobs = [];
let seq = 0;

const id32 = () => `lab_${(++seq).toString(16).padStart(32, "0")}`;

/** Move the queue forward to `now`: at most one job runs. */
function advance(now = Date.now()) {
  for (;;) {
    const running = jobs.find((j) => j.status === "running");
    if (running) {
      const age = now - Date.parse(running.started_at);
      if (age < WAIT_MS) {
        running.phase = "waiting";
        running.progress = 0;
        return;
      }
      running.phase = "generating";
      running.progress = Math.min(99, Math.floor(((age - WAIT_MS) / RUN_MS) * 100));
      if (age - WAIT_MS < RUN_MS) return;
      if (/fail/i.test(running.prompt)) {
        Object.assign(running, { status: "failed", phase: null, error: "the model runner failed (exit 3): out of memory", finished_at: new Date(now).toISOString() });
      } else {
        Object.assign(running, { status: "done", phase: null, progress: 100, finished_at: new Date(now).toISOString(), result: { hash: running.id, bytes: 1_400_000, width: 1024, height: 768 } });
      }
      continue;
    }
    const next = jobs.filter((j) => j.status === "queued").sort((a, b) => (a.created_at < b.created_at ? -1 : 1))[0];
    if (!next) return;
    Object.assign(next, { status: "running", phase: "waiting", progress: 0, started_at: new Date(now).toISOString(), attempts: next.attempts + 1 });
  }
}

function json(job, env) {
  const orig = env.items().find((i) => i.id === job.item_id);
  const queued = jobs.filter((j) => j.status === "queued").sort((a, b) => (a.created_at < b.created_at ? -1 : 1));
  const pos = job.status === "queued" ? queued.indexOf(job) + 1 + jobs.filter((j) => j.status === "running").length : null;
  const thumb = (i) => `/v1/media/items/${i}/thumb?size=1024`;
  return {
    id: job.id,
    item_id: job.item_id,
    reference_item_id: job.reference_item,
    prompt: job.prompt,
    status: job.status,
    phase: job.phase,
    progress: job.progress,
    error: job.error,
    seed: null,
    steps: null,
    model: "klein4b",
    attempts: job.attempts,
    queue_position: pos,
    created_at: job.created_at,
    started_at: job.started_at,
    finished_at: job.finished_at,
    accepted_item_id: job.accepted_item_id,
    result: job.result ?? null,
    original: orig ? { id: orig.id, title: orig.title, width: orig.width, height: orig.height, rotation: orig.rotation, thumbhash: null, trashed: !!orig.trashed_at } : null,
    urls: {
      before: ["running", "done", "failed"].includes(job.status) ? `/v1/media/lab/${job.id}/before` : orig ? thumb(orig.id) : null,
      after: job.status === "done" ? `/v1/media/lab/${job.id}/result` : job.status === "accepted" ? thumb(job.accepted_item_id) : null,
      reference: job.reference_item ? `/v1/media/items/${job.reference_item}/thumb?size=256` : null,
      result: job.status === "done" ? `/v1/media/lab/${job.id}/result` : null,
    },
  };
}

/** Returns true when the request was answered. `env`: { items(), svgFor(item, size), out(item), add(item) }. */
export async function handleLab(req, res, { rest, method, url, send, fail, readJson }, env) {
  advance();
  const create = rest.match(/^items\/([^/]+)\/lab$/);
  if (create && method === "POST") {
    const it = env.items().find((i) => i.id === decodeURIComponent(create[1]));
    if (!it) return fail(res, 404, "not_found", "no such picture"), true;
    const body = await readJson(req);
    const prompt = typeof body.prompt === "string" ? body.prompt.trim() : "";
    if (!prompt || prompt.length > 1000) return fail(res, 400, "bad_prompt", "prompt must be 1 to 1000 characters"), true;
    if (it.kind !== "photo") return fail(res, 400, "bad_kind", "only pictures can be edited"), true;
    if (jobs.filter((j) => j.status === "queued").length >= 30) return fail(res, 429, "queue_full", "the Lab queue is full"), true;
    const job = { id: id32(), item_id: it.id, reference_item: body.reference_item_id || null, prompt, status: "queued", phase: null, progress: 0, error: null, attempts: 0, created_at: new Date().toISOString(), started_at: null, finished_at: null, accepted_item_id: null };
    jobs.push(job);
    advance();
    return send(res, 201, json(job, env)), true;
  }
  if (rest === "lab" && method === "GET") {
    const wanted = (url.searchParams.get("status") ?? "").split(",").filter(Boolean);
    const itemId = url.searchParams.get("item_id");
    const list = jobs.filter((j) => (wanted.length ? wanted.includes(j.status) : j.status !== "discarded") && (!itemId || j.item_id === itemId));
    return send(res, 200, { enabled: true, jobs: list.map((j) => json(j, env)) }), true;
  }
  const one = rest.match(/^lab\/(lab_[0-9a-f]{32})(?:\/(before|result|accept|discard|retry))?$/);
  if (!one) return false;
  const job = jobs.find((j) => j.id === one[1]);
  if (!job) return fail(res, 404, "not_found", "not found"), true;
  const sub = one[2];
  const orig = env.items().find((i) => i.id === job.item_id);
  if (!sub && method === "GET") return send(res, 200, json(job, env)), true;
  if (!sub && method === "DELETE") {
    if (!["queued", "failed", "discarded"].includes(job.status)) return fail(res, 409, "conflict", `the job is ${job.status}`), true;
    jobs.splice(jobs.indexOf(job), 1);
    res.writeHead(204);
    return res.end(), true;
  }
  if ((sub === "before" || sub === "result") && method === "GET") {
    if (!orig || (sub === "result" && job.status !== "done")) return fail(res, 404, "not_found", "not found"), true;
    const base = sub === "result" ? { ...orig, hue: (orig.hue + 120) % 360 } : orig;
    res.writeHead(200, { "Content-Type": "image/svg+xml", "Cache-Control": "private, max-age=60" });
    return res.end(env.svgFor(base, 1024)), true;
  }
  if (method !== "POST") return false;
  if (sub === "accept") {
    if (job.status !== "done") return fail(res, 409, "conflict", `the job is ${job.status}`), true;
    const added = env.add(orig, job);
    Object.assign(job, { status: "accepted", accepted_item_id: added.id });
    return send(res, 200, { job: json(job, env), item: env.out(added), duplicate: false }), true;
  }
  if (sub === "discard") {
    if (!["done", "failed"].includes(job.status)) return fail(res, 409, "conflict", `the job is ${job.status}`), true;
    Object.assign(job, { status: "discarded", phase: null, result: null });
    return send(res, 200, json(job, env)), true;
  }
  if (sub === "retry") {
    if (job.status !== "failed") return fail(res, 409, "conflict", `the job is ${job.status}`), true;
    Object.assign(job, { status: "queued", error: null, progress: 0, phase: null, created_at: new Date().toISOString() });
    advance();
    return send(res, 200, json(job, env)), true;
  }
  return false;
}
