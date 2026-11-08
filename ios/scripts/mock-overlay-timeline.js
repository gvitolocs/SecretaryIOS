// ---- iOS overlay: timeline (when upstream lacks it) ----
// Unified timeline (assumed contract, until the real mock/server has it):
//   GET /v1/timeline?before=&limit=  -> {items, nextBefore}, newest first
//   GET /v1/timeline/stream          -> SSE `event: item` (id, data), pings, Last-Event-ID
//   POST /v1/chat records the exchange and adds userItemId/lucinaItemId
//   POST /v1/_mock/timeline {channel, role, text, meta?} injects a message (tests)
if (path === "/v1/timeline" && method === "GET") {
  if (!currentSession(req)) return fail(res, 401, "unauthorized", "missing or malformed device token", { "WWW-Authenticate": "Bearer" });
  const limit = Math.max(1, Math.min(100, Number(url.searchParams.get("limit") ?? 50) || 50));
  const before = url.searchParams.get("before");
  const older = [...iosTimeline].reverse().filter((item) => !before || item.id < before);
  const items = older.slice(0, limit);
  return send(res, 200, { items, nextBefore: older.length > limit ? items[items.length - 1].id : null });
}
if (path === "/v1/timeline/stream" && method === "GET") {
  if (!currentSession(req)) return fail(res, 401, "unauthorized", "missing or malformed device token", { "WWW-Authenticate": "Bearer" });
  res.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-store", Connection: "keep-alive" });
  res.write("retry: 3000\n\n");
  const lastId = req.headers["last-event-id"];
  if (lastId) for (const item of iosTimeline) if (item.id > lastId) res.write(iosEvent(item));
  iosStreams.add(res);
  const ping = setInterval(() => res.write("event: ping\ndata: {}\n\n"), 15_000);
  req.on("close", () => { clearInterval(ping); iosStreams.delete(res); });
  return;
}
if (path === "/v1/_mock/timeline" && method === "POST") {
  const body = await readJson(req);
  const item = iosAppend({
    channel: typeof body.channel === "string" ? body.channel : "telegram",
    role: body.role === "lucina" ? "lucina" : "user",
    text: typeof body.text === "string" ? body.text : "Hello from another channel",
    ...(body.meta ? { meta: body.meta } : {}),
  });
  return send(res, 200, item);
}
if (path === "/v1/chat" && method === "POST" && currentSession(req)) {
  const chunks = [];
  req.on("data", (c) => chunks.push(c));
  const client = String(req.headers["x-secretary-client"] ?? "web") === "ios" ? "ios" : "web";
  const end = res.end.bind(res);
  res.end = (payload, ...rest) => {
    try {
      if (res.statusCode === 200 && typeof payload === "string") {
        const reply = JSON.parse(payload);
        const asked = JSON.parse(Buffer.concat(chunks).toString("utf8") || "{}");
        if (typeof reply.reply === "string") {
          const key = asked.clientMessageId ? `${asked.sessionId ?? "app"}/${asked.clientMessageId}` : null;
          let ids = key ? iosChatItems.get(key) : undefined;
          if (!ids) {
            const cm = asked.clientMessageId ? { clientMessageId: asked.clientMessageId } : {};
            const user = iosAppend({ channel: client, role: "user", text: String(asked.message ?? ""), ...cm });
            const lucina = iosAppend({ channel: client, role: "lucina", text: reply.reply, replyTo: user.id, ...cm });
            ids = { userItemId: user.id, lucinaItemId: lucina.id };
            if (key) iosChatItems.set(key, ids);
          }
          payload = JSON.stringify({ ...reply, ...ids });
        }
      }
    } catch {
      // leave the upstream answer untouched
    }
    return end(payload, ...rest);
  };
  // fall through to the upstream chat handler
}
// ---- end iOS overlay ----
