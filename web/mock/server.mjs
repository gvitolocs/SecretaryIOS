#!/usr/bin/env node
// In-memory mock of the Secretary server API (docs/server-api.md) for local development.
// No dependencies. Access key: "dev" (override with MOCK_ACCESS_KEY).
//
//   node mock/server.mjs            # listens on 127.0.0.1:8791
//   MOCK_PORT=9000 node mock/server.mjs
//
// Chat triggers (anywhere in the message, case-insensitive):
//   "offline"  -> 502 lucina_unreachable      "timeout" -> 504 lucina_timeout
//   "fail"     -> 500 internal                "link"    -> reply with URLs
//   "job" / "order" / "book" / "image" -> reply with a job (queued -> running -> done)
//   "remind"   -> schedule_reminder reply with a reminder object (no job)
//   "jobfail"  -> a job that ends in error
// Shopping: items whose name contains "stubborn" refuse PATCH (500), to test rollback.
// Jobs: "image" in the message makes a generate_image job with an imageUrl.
// POST /v1/_mock/lucina {"online": false} toggles Lucina's health (CSRF header needed).

import http from "node:http";
import { randomUUID } from "node:crypto";

const PORT = Number(process.env.MOCK_PORT ?? 8791);
const HOST = process.env.MOCK_HOST ?? "127.0.0.1";
const ACCESS_KEY = process.env.MOCK_ACCESS_KEY ?? "dev";
const COOKIE = "sec_session";
const MAX_BODY = 256 * 1024;
const RATE_WINDOW_MS = 60_000;
const RATE_MAX_FAILURES = 5;

/** @type {Map<string, {deviceName: string}>} */
const sessions = new Map();
/** @type {Map<string, number[]>} */
const loginFailures = new Map();
/** @type {Map<string, {id: string, action: string, createdAt: number, fail: boolean, summary: string}>} */
const jobs = new Map();
/** @type {Map<string, object>} clientMessageId -> response, for idempotent retries */
const chatReplies = new Map();
let lucinaOnline = process.env.MOCK_LUCINA_OFFLINE !== "1";

const iso = () => new Date().toISOString();
const shopItem = (rawName, product, category, status) => ({
  id: randomUUID(), product, rawName, category, status, createdAt: iso(), updatedAt: iso(),
});
let shopping = [
  shopItem("latte d'avena", "oat milk", "dairy", "needed"),
  shopItem("uova x6", "eggs", "dairy", "needed"),
  shopItem("caffè in grani", "espresso beans", "pantry", "needed"),
  shopItem("basilico", "basil", "produce", "purchased"),
  shopItem("stubborn sponge (refuses to toggle)", "sponge", "household", "needed"),
];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function send(res, status, body, headers = {}) {
  const payload = body === undefined ? "" : JSON.stringify(body);
  res.writeHead(status, {
    "Content-Type": "application/json; charset=utf-8",
    "Cache-Control": "no-store",
    ...headers,
  });
  res.end(payload);
}

function fail(res, status, code, message, headers = {}) {
  send(res, status, { error: { code, message } }, headers);
}

function readJson(req) {
  return new Promise((resolve, reject) => {
    let size = 0;
    const chunks = [];
    req.on("data", (c) => {
      size += c.length;
      if (size > MAX_BODY) {
        reject(Object.assign(new Error("body too large"), { status: 413 }));
        req.destroy();
        return;
      }
      chunks.push(c);
    });
    req.on("end", () => {
      const text = Buffer.concat(chunks).toString("utf8");
      if (!text) return resolve({});
      try {
        resolve(JSON.parse(text));
      } catch {
        reject(Object.assign(new Error("invalid JSON"), { status: 400 }));
      }
    });
    req.on("error", reject);
  });
}

function cookies(req) {
  const out = {};
  for (const part of (req.headers.cookie ?? "").split(";")) {
    const i = part.indexOf("=");
    if (i > 0) out[part.slice(0, i).trim()] = decodeURIComponent(part.slice(i + 1).trim());
  }
  return out;
}

function currentSession(req) {
  const sid = cookies(req)[COOKIE];
  return sid ? (sessions.get(sid) ?? null) : null;
}

function jobView(job) {
  const age = Date.now() - job.createdAt;
  const status = age < 2000 ? "queued" : age < 7000 ? "running" : job.fail ? "error" : "done";
  const view = {
    id: job.id,
    status,
    action: job.action,
    createdAt: new Date(job.createdAt).toISOString(),
    updatedAt: new Date(Math.min(Date.now(), job.createdAt + 7000)).toISOString(),
  };
  view.result = status === "done" ? job.summary : null;
  view.error = status === "error" ? "The store's site refused the request (mock failure)." : null;
  view.imageUrl = status === "done" && job.action === "generate_image" ? "https://example.com/mock-image.png" : null;
  view.sessionId = job.sessionId;
  view.clientMessageId = job.clientMessageId;
  return view;
}

function lucinaReply(message) {
  const m = message.toLowerCase();
  if (m.includes("link")) {
    return "Here you are:\nhttps://example.com/recipes/risotto (the one you liked)\nand www.example.org/list.\n\nA javascript:alert(1) is never linked.";
  }
  if (/\b(hi|hello|hey)\b/.test(m)) return "Good day. I'm here, and I remember where we left off.\nWhat do you need?";
  return `Understood: "${message}".\nI'll keep it in mind.`;
}

async function handleChat(req, res) {
  const body = await readJson(req);
  const message = typeof body.message === "string" ? body.message : "";
  const sessionId = body.sessionId === undefined ? "app" : body.sessionId;
  const clientMessageId = body.clientMessageId ?? null;
  if (!message.trim() || message.length > 4000) return fail(res, 400, "bad_request", "message must be 1-4000 characters");
  if (/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/.test(message)) {
    return fail(res, 400, "bad_request", "message contains control characters");
  }
  if (typeof sessionId !== "string" || !/^[A-Za-z0-9_-]{1,64}$/.test(sessionId)) {
    return fail(res, 400, "bad_request", "sessionId must match [A-Za-z0-9_-]{1,64}");
  }
  if (clientMessageId !== null && (typeof clientMessageId !== "string" || !/^[A-Za-z0-9_.:-]{1,128}$/.test(clientMessageId))) {
    return fail(res, 400, "bad_request", "clientMessageId must match [A-Za-z0-9_.:-]{1,128}");
  }

  const key = clientMessageId ? `${sessionId}/${clientMessageId}` : null;
  const cached = key ? chatReplies.get(key) : undefined;
  if (cached) return send(res, 200, { ...cached, duplicate: true });

  const m = message.toLowerCase();
  await sleep(600 + Math.random() * 900);
  if (!lucinaOnline || m.includes("offline")) {
    return fail(res, 502, "lucina_unreachable", "Lucina is not reachable right now.");
  }
  if (m.includes("timeout")) {
    await sleep(1000);
    return fail(res, 504, "lucina_timeout", "Lucina did not answer in time.");
  }
  if (/\bfail\b/.test(m)) return fail(res, 500, "internal", "Something broke (mock).");

  let reply;
  const ids = { sessionId: `secretary_${sessionId}`, clientMessageId };
  if (/(job|order|book|image)/.test(m)) {
    const id = randomUUID();
    const action = m.includes("image") ? "generate_image" : m.includes("order") ? "spreadsheet_update_task" : "cursor_task";
    jobs.set(id, {
      id,
      action,
      createdAt: Date.now(),
      fail: m.includes("jobfail"),
      summary: `Finished "${message}".\nDetails: https://example.com/jobs/${id.slice(0, 8)}`,
      ...ids,
    });
    reply = { reply: "On it. I'll report back here when it's done.", action, job: { id, status: "queued" }, ...ids };
  } else if (m.includes("remind")) {
    const dueAt = new Date(Date.now() + 3600_000).toISOString();
    reply = {
      reply: `Reminder set for ${dueAt} (Europe/Rome): ${message}`,
      action: "schedule_reminder",
      job: null,
      ...ids,
      reminder: { id: randomUUID(), text: message, dueAt, timeZone: "Europe/Rome", status: "pending", source: "secretary" },
    };
  } else {
    reply = { reply: lucinaReply(message), action: "reply", job: null, ...ids };
  }
  if (key) chatReplies.set(key, reply);
  send(res, 200, reply);
}

async function handleLogin(req, res) {
  const ip = req.socket.remoteAddress ?? "?";
  const now = Date.now();
  const recent = (loginFailures.get(ip) ?? []).filter((t) => now - t < RATE_WINDOW_MS);
  loginFailures.set(ip, recent);
  if (recent.length >= RATE_MAX_FAILURES) {
    const retry = Math.ceil((RATE_WINDOW_MS - (now - (recent[0] ?? now))) / 1000);
    return fail(res, 429, "rate_limited", "Too many attempts.", { "Retry-After": String(retry) });
  }
  const body = await readJson(req);
  await sleep(250);
  if (typeof body.token !== "string" || !body.token) return fail(res, 400, "bad_request", "token is required");
  if (body.token !== ACCESS_KEY) {
    recent.push(now);
    return fail(res, 401, "unauthorized", "unknown device token", { "WWW-Authenticate": "Bearer" });
  }
  loginFailures.delete(ip);
  const sid = randomUUID();
  const ua = String(req.headers["user-agent"] ?? "");
  sessions.set(sid, { deviceName: /iPhone/.test(ua) ? "iPhone" : "Browser" });
  send(res, 200, { authenticated: true, deviceName: sessions.get(sid).deviceName }, {
    "Set-Cookie": `${COOKIE}=${sid}; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000`,
  });
}

async function route(req, res) {
  const url = new URL(req.url ?? "/", "http://mock.local");
  const path = url.pathname.replace(/\/+$/, "") || "/";
  const method = req.method ?? "GET";

  if (!path.startsWith("/v1/")) return fail(res, 404, "not_found", "Not found.");
  const writes = ["POST", "PUT", "PATCH", "DELETE"].includes(method);
  // POST /v1/session is exempt (its JSON content type already forces a preflight).
  if (writes && !(method === "POST" && path === "/v1/session") && req.headers["x-secretary-csrf"] !== "1") {
    return fail(res, 403, "csrf", "missing X-Secretary-CSRF header");
  }
  if (["POST", "PUT", "PATCH"].includes(method) && !String(req.headers["content-type"] ?? "").startsWith("application/json")) {
    return fail(res, 415, "unsupported_media_type", "Content-Type must be application/json");
  }

  if (path === "/v1/health" && method === "GET") {
    return send(res, 200, {
      ok: true,
      version: "mock-0.1.0",
      lucina: lucinaOnline
        ? { reachable: true, latencyMs: Math.round(40 + Math.random() * 160), apiVersion: 1 }
        : { reachable: false, latencyMs: null, error: "lucina_unreachable" },
    });
  }

  if (path === "/v1/session") {
    if (method === "GET") {
      const s = currentSession(req);
      return send(res, 200, { authenticated: !!s, deviceName: s?.deviceName ?? null });
    }
    if (method === "POST") return handleLogin(req, res);
    if (method === "DELETE") {
      const sid = cookies(req)[COOKIE];
      if (sid) sessions.delete(sid);
      return send(res, 200, { authenticated: false, deviceName: null }, {
        "Set-Cookie": `${COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0`,
      });
    }
    return fail(res, 405, "method_not_allowed", "Method not allowed.");
  }

  if (path === "/v1/_mock/lucina" && method === "POST") {
    const body = await readJson(req);
    lucinaOnline = body.online !== false;
    return send(res, 200, { online: lucinaOnline });
  }

  if (!currentSession(req)) {
    return fail(res, 401, "unauthorized", "session expired or invalid", { "WWW-Authenticate": "Bearer" });
  }

  if (path === "/v1/chat" && method === "POST") return handleChat(req, res);

  const jobMatch = path.match(/^\/v1\/jobs\/([^/]+)$/);
  if (jobMatch && method === "GET") {
    const job = jobs.get(decodeURIComponent(jobMatch[1]));
    if (!job) return fail(res, 404, "job_not_found", "No such job.");
    return send(res, 200, jobView(job));
  }

  if (path === "/v1/shopping") {
    if (method === "GET") {
      await sleep(150);
      const rank = (i) => (i.status === "needed" ? 0 : 1);
      return send(res, 200, [...shopping].sort((a, b) => rank(a) - rank(b)));
    }
    if (method === "POST") {
      const body = await readJson(req);
      const rawName = typeof body.rawName === "string" ? body.rawName.trim() : "";
      if (!rawName || rawName.length > 200) return fail(res, 400, "bad_request", "rawName must be 1-200 characters");
      const product = typeof body.product === "string" && body.product.trim() ? body.product.trim() : rawName.toLowerCase();
      await sleep(300);
      const item = shopItem(rawName, product, null, "needed");
      shopping = [...shopping, item];
      return send(res, 201, item);
    }
    return fail(res, 405, "method_not_allowed", "Method not allowed.");
  }

  const itemMatch = path.match(/^\/v1\/shopping\/([^/]+)$/);
  if (itemMatch && method === "PATCH") {
    const id = decodeURIComponent(itemMatch[1]);
    const item = shopping.find((i) => i.id === id);
    if (!item) return fail(res, 404, "item_not_found", "No such item.");
    const body = await readJson(req);
    await sleep(250);
    if (body.status !== "needed" && body.status !== "purchased") {
      return fail(res, 400, "invalid_status", 'status must be "needed" or "purchased"');
    }
    if (/stubborn/i.test(item.rawName)) return fail(res, 500, "internal", "This item refuses to change (mock).");
    item.status = body.status;
    item.updatedAt = iso();
    // Like Lucina: PATCH answers only {id, product, status}, product = raw name.
    return send(res, 200, { id: item.id, product: item.rawName, status: item.status });
  }

  return fail(res, 404, "not_found", "Not found.");
}

const server = http.createServer((req, res) => {
  route(req, res).catch((err) => {
    const status = typeof err?.status === "number" ? err.status : 500;
    const code = status === 500 ? "internal" : status === 413 ? "payload_too_large" : "invalid_json";
    if (!res.headersSent) fail(res, status, code, String(err?.message ?? err));
    else res.end();
  });
});

server.listen(PORT, HOST, () => {
  console.log(`[mock] Secretary API on http://${HOST}:${PORT} (access key: "${ACCESS_KEY}")`);
});

for (const sig of ["SIGINT", "SIGTERM"]) {
  process.on(sig, () => server.close(() => process.exit(0)));
}
