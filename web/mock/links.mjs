// Links hub and calendar for the mock server (docs/server-api.md, "Links" and "Calendar").
// In memory, no real Google: "Connect" redirects straight back with ?google=connected.

const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const iso = (ms) => new Date(ms).toISOString();
const ago = (ms) => iso(Date.now() - ms);

let googleConnected = true;
let googleReconnect = false;
const services = new Set(["calendar"]);
let ics = [
  { id: "ics_seriea", label: "Serie A", host: "calendar.google.com", feedName: "Serie A 2026/27", count: 38, synced: ago(6 * 60_000) },
  { id: "ics_work", label: "Work (Outlook)", host: "outlook.office365.com", feedName: "Calendar", count: 112, synced: ago(4 * 60_000), error: null },
];
let custom = [
  { id: "lnk_bank", kind: "bookmark", label: "Bank", url: "https://bank.example", host: "bank.example", icon: "🏦", tags: ["money"], createdAt: ago(9 * DAY) },
  { id: "lnk_grafana", kind: "bookmark", label: "Grafana", url: "https://grafana.example/d/oracle", host: "grafana.example", icon: "📈", tags: ["infra", "oracle"], createdAt: ago(3 * DAY) },
  { id: "lnk_notion", kind: "mcp", label: "Notion MCP", url: null, host: "mcp.notion.com", icon: null, tags: ["notes"], createdAt: ago(DAY) },
];
let proposals = [
  {
    id: "prop_1",
    event: { title: "Padel with Marco", start: iso(dayAt(3, 18)), end: iso(dayAt(3, 19, 30)), location: "Club Nord" },
    from: "mcp:claude-code",
    createdAt: ago(20 * 60_000),
    expiresAtMs: Date.now() + 7 * DAY,
  },
];
let lastSync = ago(2 * 60_000);

function dayAt(offset, h, m = 0) {
  const d = new Date();
  d.setDate(d.getDate() + offset);
  d.setHours(h, m, 0, 0);
  return d.getTime();
}

function ymd(offset) {
  const d = new Date(dayAt(offset, 12));
  return d.toISOString().slice(0, 10);
}

function events() {
  const ev = (id, source, calendar, title, start, end, extra = {}) => ({
    id,
    source,
    sourceId: id.split(":").pop(),
    calendar,
    title,
    start: iso(start),
    end: iso(end),
    allDay: false,
    startMs: start,
    endMs: end,
    recurring: false,
    editable: source === "google",
    ...extra,
  });
  const list = [
    ev("google:dentist", "google", "Google", "Dentist", dayAt(0, 9, 30), dayAt(0, 10)),
    ev("iphone:EK1", "iphone", "Casa", "Call mum", dayAt(0, 18, 30), dayAt(0, 19)),
    ev("google:gym_1", "google", "Google", "Gym: push day", dayAt(1, 7), dayAt(1, 8), { recurring: true }),
    ev("ics:ics_work:a1", "ics", "Work (Outlook)", "Sprint planning", dayAt(1, 10), dayAt(1, 11, 30)),
    {
      id: "iphone:EK2",
      source: "iphone",
      sourceId: "EK2",
      calendar: "Compleanni",
      title: "Anna's birthday",
      start: ymd(2),
      end: ymd(3),
      allDay: true,
      startMs: dayAt(2, 0),
      endMs: dayAt(3, 0),
      recurring: true,
      editable: false,
    },
    ev("ics:ics_seriea:b2", "ics", "Serie A", "Inter – Napoli", dayAt(2, 20, 45), dayAt(2, 22, 45)),
    ev("google:lunch", "google", "Google", "Lunch with Anna", dayAt(4, 12, 30), dayAt(4, 13, 30)),
  ];
  return googleConnected ? list : list.filter((e) => e.source !== "google");
}

function overview() {
  const gStatus = !googleConnected ? "disconnected" : googleReconnect ? "reconnect" : "connected";
  const gcard = (id, service, name) => {
    const has = googleConnected && services.has(service);
    const status = has ? (googleReconnect ? "reconnect" : "connected") : service === "calendar" ? "disconnected" : "available";
    return {
      id,
      kind: "google",
      service,
      name,
      status,
      account: has ? "owner@example.com" : null,
      scopes: has
        ? service === "calendar"
          ? ["calendar.events", "calendar.readonly"]
          : [`${service}.readonly`]
        : [],
      lastSyncAt: has && service === "calendar" ? lastSync : null,
      count: has && service === "calendar" ? 57 : undefined,
      actions:
        status === "connected"
          ? service === "calendar"
            ? ["sync", "disconnect"]
            : ["disconnect"]
          : status === "reconnect"
            ? ["reconnect", "disconnect"]
            : ["connect"],
    };
  };
  const now = Date.now();
  return {
    timeZone: "Europe/Rome",
    google: { configured: true, status: gStatus, account: googleConnected ? "owner@example.com" : null, services: [...services] },
    groups: [
      {
        id: "calendars",
        cards: [
          gcard("google-calendar", "calendar", "Google Calendar"),
          {
            id: "iphone-calendar",
            kind: "iphone",
            name: "iPhone Calendar",
            status: "connected",
            account: "Giuseppe's iPhone",
            lastSyncAt: ago(11 * 60_000),
            count: 24,
            scopes: ["eventkit.read", "eventkit.write"],
            actions: [],
          },
          ...ics.map((s) => ({
            id: `ics:${s.id}`,
            kind: "ics",
            name: s.label,
            status: s.error ? "error" : "connected",
            account: s.host,
            lastSyncAt: s.synced,
            count: s.count,
            detail: s.feedName,
            error: s.error ?? null,
            scopes: ["read-only"],
            actions: ["sync", "remove"],
          })),
        ],
      },
      {
        id: "channels",
        cards: [
          { id: "channel:telegram", kind: "channel", name: "Telegram", status: "connected", lastSyncAt: ago(14 * 60_000), actions: [] },
          { id: "channel:discord", kind: "channel", name: "Discord", status: "connected", lastSyncAt: ago(3 * HOUR), actions: [] },
          { id: "channel:alexa", kind: "channel", name: "Alexa", status: "planned", actions: [] },
        ],
      },
      {
        id: "ai",
        cards: [
          { id: "ai:claude-code", kind: "ai", name: "Claude Code", status: "connected", account: "MCP token", scopes: ["chat", "read", "write"], lastSyncAt: ago(40 * 60_000), actions: [] },
          { id: "ai:chatgpt", kind: "ai", name: "ChatGPT", status: "connected", account: "OAuth", scopes: ["chat", "read"], lastSyncAt: ago(2 * DAY), actions: [] },
          { id: "ai:grok", kind: "ai", name: "Grok Bot", status: "connected", account: "OAuth", scopes: ["read"], lastSyncAt: ago(5 * HOUR), actions: [] },
        ],
      },
      { id: "google", cards: [gcard("google-gmail", "gmail", "Gmail"), gcard("google-drive", "drive", "Google Drive")] },
      {
        id: "custom",
        cards: custom.map((c) => ({
          id: `custom:${c.id}`,
          kind: c.kind,
          name: c.label,
          status: c.kind === "mcp" ? "saved" : "connected",
          url: c.kind === "bookmark" ? c.url : undefined,
          account: c.host,
          icon: c.icon,
          tags: c.tags,
          lastSyncAt: c.createdAt,
          actions: ["edit", "remove"],
        })),
      },
    ],
    proposals: proposals.filter((p) => p.expiresAtMs > now),
  };
}

export async function handleLinks(req, res, { path, method, url, send, fail, readJson }) {
  if (path === "/v1/links" && method === "GET") return send(res, 200, overview()), true;
  if (path === "/v1/links/google/connect" && method === "POST") {
    const body = await readJson(req);
    for (const s of body.services ?? ["calendar"]) services.add(s);
    googleConnected = true;
    googleReconnect = false;
    return send(res, 200, { authorizeUrl: "/links?google=connected" }), true;
  }
  if (path === "/v1/links/google" && method === "DELETE") {
    googleConnected = false;
    services.clear();
    services.add("calendar");
    return send(res, 200, { disconnected: true, revokedAtGoogle: true }), true;
  }
  if (path === "/v1/_mock/links/reconnect" && method === "POST") {
    googleReconnect = true;
    return send(res, 200, { reconnect: true }), true;
  }
  if (path === "/v1/links/ics" && method === "POST") {
    const body = await readJson(req);
    const raw = String(body.url ?? "").trim();
    if (!/^(https|webcals?):\/\//i.test(raw)) return fail(res, 400, "bad_request", "url: use the https:// (or webcal://) address of the calendar"), true;
    const host = raw.replace(/^[a-z]+:\/\//i, "").split("/")[0];
    const id = `ics_${Math.random().toString(36).slice(2, 8)}`;
    ics = [...ics, { id, label: body.label || host, host, feedName: null, count: 12, synced: iso(Date.now()) }];
    return send(res, 201, { id, label: body.label || host, host, eventCount: 12, feedName: null }), true;
  }
  const icsMatch = path.match(/^\/v1\/links\/ics\/([^/]+)$/);
  if (icsMatch && method === "DELETE") {
    ics = ics.filter((s) => s.id !== decodeURIComponent(icsMatch[1]));
    return send(res, 200, { removed: true }), true;
  }
  if (path === "/v1/links/custom" && method === "POST") {
    const body = await readJson(req);
    const label = String(body.label ?? "").trim();
    const link = String(body.url ?? "").trim();
    if (!label || !/^https?:\/\//.test(link)) return fail(res, 400, "bad_request", "label and an http(s) url are required"), true;
    const id = `lnk_${Math.random().toString(36).slice(2, 8)}`;
    const host = link.replace(/^https?:\/\//, "").split("/")[0];
    custom = [...custom, { id, kind: body.kind === "mcp" ? "mcp" : "bookmark", label, url: body.kind === "mcp" ? null : link, host, icon: body.icon ?? null, tags: body.tags ?? [], createdAt: iso(Date.now()) }];
    return send(res, 201, { id, label }), true;
  }
  const customMatch = path.match(/^\/v1\/links\/custom\/([^/]+)$/);
  if (customMatch && method === "DELETE") {
    custom = custom.filter((c) => c.id !== decodeURIComponent(customMatch[1]));
    return send(res, 200, { removed: true }), true;
  }
  if (path === "/v1/calendar/sync" && method === "POST") {
    lastSync = iso(Date.now());
    return send(res, 200, { google: { ok: googleConnected }, ics: ics.map((s) => ({ id: s.id, ok: true })) }), true;
  }
  if (path === "/v1/calendar/events" && method === "GET") {
    return send(res, 200, { from: iso(dayAt(0, 0)), to: iso(dayAt(Number(url.searchParams.get("days") ?? 7), 0)), timeZone: "Europe/Rome", events: events() }), true;
  }
  const propMatch = path.match(/^\/v1\/calendar\/proposals\/([^/]+)(\/accept)?$/);
  if (propMatch && ((method === "POST" && propMatch[2]) || (method === "DELETE" && !propMatch[2]))) {
    proposals = proposals.filter((p) => p.id !== decodeURIComponent(propMatch[1]));
    return send(res, method === "POST" ? 201 : 200, { ok: true }), true;
  }
  return false;
}
