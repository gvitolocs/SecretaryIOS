// ---- iOS overlay: device bridge (when upstream lacks it) ----
// POST /v1/device/events {events}, POST /v1/device/commands/{id}/result, POST /v1/device/state.
// GET /v1/_mock/device returns what arrived (tests). Commands are sent with
// POST /v1/_mock/timeline {role:"lucina", text, meta:{kind:"device_command", command:{id,type,args}}}.
if (path.startsWith("/v1/device/") || path === "/v1/_mock/device") {
  globalThis.iosDevice ??= { events: [], seen: new Set(), results: {}, state: null };
  const store = globalThis.iosDevice;
  if (path === "/v1/_mock/device" && method === "GET") {
    return send(res, 200, { events: store.events, results: store.results, state: store.state });
  }
  if (!currentSession(req)) return fail(res, 401, "unauthorized", "missing or malformed device token", { "WWW-Authenticate": "Bearer" });
  const body = await readJson(req);
  if (path === "/v1/device/events" && method === "POST") {
    const events = Array.isArray(body.events) ? body.events : [];
    if (events.length < 1 || events.length > 500) return fail(res, 400, "bad_request", "events must have 1-500 entries");
    const duplicates = [];
    let accepted = 0;
    for (const e of events) {
      if (store.seen.has(e.id)) { duplicates.push(e.id); continue; }
      store.seen.add(e.id);
      store.events.push(e);
      accepted++;
    }
    return send(res, 200, { accepted, duplicates, results: [] });
  }
  const result = path.match(/^\/v1\/device\/commands\/([^/]+)\/result$/);
  if (result && method === "POST") {
    if (typeof body.ok !== "boolean") return fail(res, 400, "bad_request", "ok is required");
    store.results[decodeURIComponent(result[1])] = body;
    return send(res, 200, { ok: true });
  }
  if (path === "/v1/device/state" && method === "POST") {
    store.state = body;
    return send(res, 200, { ok: true });
  }
}
// ---- end iOS device overlay ----
