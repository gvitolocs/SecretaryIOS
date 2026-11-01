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
  {
    id: "prop_2",
    event: { title: "Dinner at Gaia", start: iso(dayAt(5, 19, 30)), end: iso(dayAt(5, 21)), location: "Gaia, Aarhus" },
    from: "Lucina",
    reason: "Google Calendar isn't connected",
    createdAt: ago(3 * HOUR),
    expiresAtMs: Date.now() + 7 * DAY,
  },
];
let lastSync = ago(2 * 60_000);
/** Todoist link (POST /v1/links/todoist/token); connected when MOCK_TASKS_URL proxies a real server. */
let todoist = process.env.MOCK_TASKS_URL ? { account: "owner@example.com" } : null;

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

// ---- calendar: Lucina's own calendar (writable, in memory) + read-only iPhone/ICS events + Google ----

const LUCINA_CAL = "Lucina";
let evSeq = 0;
const evId = () => `lucina:ev_${(++evSeq).toString(36)}${Math.random().toString(36).slice(2, 6)}`;

function timed(id, source, calendar, title, start, end, extra = {}) {
  return {
    id,
    source,
    sourceId: id.replace(/^[a-z]+:/, ""),
    calendar,
    title,
    start: iso(start),
    end: iso(end),
    allDay: false,
    startMs: start,
    endMs: end,
    recurring: false,
    editable: source === "lucina" || source === "google",
    ...extra,
  };
}

function allDay(id, source, calendar, title, fromOffset, days, extra = {}) {
  return {
    id,
    source,
    sourceId: id.replace(/^[a-z]+:/, ""),
    calendar,
    title,
    start: ymd(fromOffset),
    end: ymd(fromOffset + days),
    allDay: true,
    startMs: dayAt(fromOffset, 0),
    endMs: dayAt(fromOffset + days, 0),
    recurring: false,
    editable: source === "lucina" || source === "google",
    ...extra,
  };
}

/** Days from today to the next given weekday (0 = Sunday), plus `weeks` weeks. */
function weekday(dow, weeks) {
  const d = new Date();
  return ((dow - d.getDay() + 7) % 7) + weeks * 7;
}

const own = [
  timed(evId(), "lucina", LUCINA_CAL, "Coffee with Sofie", dayAt(0, 8), dayAt(0, 8, 30), { addedBy: "Lucina" }),
  timed(evId(), "lucina", LUCINA_CAL, "Dentist", dayAt(0, 15), dayAt(0, 16), {
    location: "Tandlægerne Aarhus, Søndergade 20, 8000 Aarhus C",
    description: "Check-up. Bring the insurance card.",
    addedBy: "Lucina",
  }),
  timed(evId(), "lucina", LUCINA_CAL, "Call the bank", dayAt(0, 15, 30), dayAt(0, 16), { addedBy: "Web" }),
  timed(evId(), "lucina", LUCINA_CAL, "Design review", dayAt(1, 14), dayAt(1, 15), {
    conferenceUrl: "https://meet.google.com/abc-defg-hij",
    description: "<p>Agenda:</p><ul><li>Calendar tab</li><li>Next steps</li></ul>",
    attendees: ["sofie@example.com", "marco@example.com"],
    addedBy: "Mac",
  }),
  // Apple Calendar's "Founders' Entry – Kitchen" (the event sheet's reference): a phone number, a Teams link and an
  // address in the notes, overlapping Monday's Software Security seminar.
  timed(evId(), "lucina", LUCINA_CAL, "Founders' Entry – Kitchen", dayAt(weekday(1, 0), 14), dayAt(weekday(1, 0), 15, 30), {
    location: "Boiler, Kitchen, Universitetsbyen 76, 8000 Aarhus C",
    description:
      "In-person welcome to join Kitchen (required for AU Launch). Program: intro, startup presentations, house tour. " +
      "You are already registered via Eventværk. SoftSec class the same day is 12:00–16:00.\n\n" +
      "Can't make it? Join on Teams: https://teams.microsoft.com/l/meetup-join/19%3ameeting_kitchen%40thread.v2/0\n" +
      "Questions: kitchen@au.dk or call Anna on 52 20 01 44.\nThe Kitchen, Universitetsbyen 76, 8000 Aarhus C.",
    alarms: [60, 1440],
    addedBy: "Lucina",
  }),
  timed(evId(), "lucina", LUCINA_CAL, "Lunch with Anna", dayAt(4, 12, 30), dayAt(4, 13, 30), { location: "Gaia, Aarhus", addedBy: "Lucina" }),
  allDay(evId(), "lucina", LUCINA_CAL, "Trip to Rome", 8, 3, { addedBy: "iPhone" }),
];
for (let w = -5; w < 10; w++) {
  for (const dow of [1, 4]) {
    const o = weekday(dow, w);
    own.push(timed(evId(), "lucina", LUCINA_CAL, "Gym: push day", dayAt(o, 18), dayAt(o, 19, 15), { recurring: true, location: "SATS Aarhus C", addedBy: "Lucina" }));
  }
}

function readOnly() {
  const list = [];
  for (let w = -5; w < 10; w++) {
    const fri = weekday(5, w);
    const mon = weekday(1, w);
    const tue = weekday(2, w);
    const au = "Aarhus Universitet - Personal timetable: 810677";
    const at = (o, h) => dayAt(o, h);
    list.push(timed(`iphone:317472A7:eh-${w}@${at(fri, 8)}`, "iphone", au, "Ethical Hacking", at(fri, 8), at(fri, 12), { location: "5522-115 Undervisningslokale", recurring: true }));
    list.push(timed(`iphone:317472A7:sp-${w}@${at(mon, 8)}`, "iphone", au, "Security and Privacy", at(mon, 8), at(mon, 12), { location: "5789-003B Seminarrum", recurring: true }));
    list.push(timed(`iphone:317472A7:ss-${w}@${at(mon, 12)}`, "iphone", au, "Software Security", at(mon, 12), at(mon, 16), { location: "5220-014 Seminarlokale", recurring: true }));
    list.push(timed(`iphone:317472A7:cn-${w}@${at(tue, 8)}`, "iphone", au, "Communications and Network Security", at(tue, 8), at(tue, 12), { location: "5125-140 Seminarrum", recurring: true }));
  }
  list.push(timed(`iphone:317472A7:mentor@${dayAt(0, 13, 30)}`, "iphone", "Home", "Mentor", dayAt(0, 13, 30), dayAt(0, 14, 30), {
    description: "Join: https://meet.google.com/xyz-abcd-efg",
  }));
  list.push(allDay(`iphone:71255870:halloween@${dayAt(1, 0)}`, "iphone", "Holidays in Denmark", "Halloween", 1, 1));
  list.push(allDay(`iphone:6F79594D:allsaints@${dayAt(2, 0)}`, "iphone", "Holidays in Italy", "All Saints' Day", 2, 1));
  list.push(allDay("iphone:EK2", "iphone", "Compleanni", "Anna's birthday", 2, 1, { recurring: true }));
  list.push(timed("ics:ics_seriea:b2", "ics", "Serie A", "Inter – Napoli", dayAt(2, 20, 45), dayAt(2, 22, 45)));
  list.push(timed("ics:ics_work:a1", "ics", "Work (Outlook)", "Sprint planning", dayAt(1, 10), dayAt(1, 11, 30)));
  return list;
}

const google = [timed("google:padel", "google", "Google", "Padel", dayAt(3, 19), dayAt(3, 20, 30), { url: "https://calendar.google.com/calendar/event?eid=cGFkZWwgb3duZXJAZXhhbXBsZS5jb20" })];

function events() {
  const list = [...own, ...readOnly(), ...(googleConnected ? google : [])];
  return list.sort((a, b) => a.startMs - b.startMs);
}

function parseBound(v, end) {
  if (!v) return null;
  if (/^\d{4}-\d{2}-\d{2}$/.test(v)) {
    const [y, m, d] = v.split("-").map(Number);
    return new Date(y, m - 1, d + (end ? 1 : 0)).getTime();
  }
  const t = Date.parse(v);
  return Number.isNaN(t) ? null : t;
}

/** A POST/PATCH body's times → the event's fields (null when they don't parse). */
function timesFrom(body, prev) {
  const start = body.start ?? prev?.start;
  if (typeof start !== "string") return null;
  const isDate = /^\d{4}-\d{2}-\d{2}$/.test(start);
  const allDayEv = body.allDay ?? isDate;
  if (allDayEv) {
    if (!isDate) return null;
    const [y, m, d] = start.split("-").map(Number);
    const s = new Date(y, m - 1, d).getTime();
    let end = body.end;
    if (typeof end !== "string" && prev?.allDay && body.start) {
      const days = Math.round((prev.endMs - prev.startMs) / DAY);
      end = new Date(y, m - 1, d + days, 12).toISOString().slice(0, 10);
    }
    if (typeof end !== "string") end = new Date(y, m - 1, d + 1, 12).toISOString().slice(0, 10);
    const [ey, em, ed] = end.split("-").map(Number);
    const e = new Date(ey, em - 1, ed).getTime();
    if (!(e > s)) return null;
    return { allDay: true, start, end, startMs: s, endMs: e };
  }
  const s = Date.parse(start);
  if (Number.isNaN(s)) return null;
  let e = typeof body.end === "string" ? Date.parse(body.end) : NaN;
  if (Number.isNaN(e)) e = prev && !prev.allDay ? s + (prev.endMs - prev.startMs) : s + HOUR;
  if (!(e > s)) return null;
  return { allDay: false, start: iso(s), end: iso(e), startMs: s, endMs: e };
}

function textFields(body, ev) {
  for (const k of ["location", "description", "conferenceUrl"]) {
    if (k in body) {
      const v = typeof body[k] === "string" ? body[k].trim() : "";
      if (v) ev[k] = v;
      else delete ev[k];
    }
  }
  // Event settings, as secretary-server validates them.
  if ("alarms" in body) {
    if (body.alarms === null) {
      ev.alarms = [];
      ev.alarmsDefault = true;
    } else if (Array.isArray(body.alarms) && body.alarms.length <= 5 && body.alarms.every((m) => Number.isInteger(m) && m >= 0 && m <= 40320)) {
      ev.alarms = [...new Set(body.alarms)].sort((a, b) => a - b);
      delete ev.alarmsDefault;
    } else return "alarms: minutes before the start, 0-40320, at most 5";
  }
  if ("free" in body) {
    if (body.free === true) ev.free = true;
    else delete ev.free;
  }
  if ("travelMin" in body) {
    if (Number.isInteger(body.travelMin) && body.travelMin > 0 && body.travelMin <= 1440) ev.travelMin = body.travelMin;
    else delete ev.travelMin;
  }
  if ("leaveAlert" in body) {
    if (body.leaveAlert === true) ev.leaveAlert = true;
    else delete ev.leaveAlert;
  }
  return null;
}

function createOwn(body, addedBy) {
  const title = typeof body.title === "string" ? body.title.trim() : "";
  if (!title) return { error: "title: required" };
  const times = timesFrom(body, null);
  if (!times) return { error: "start/end: a valid range" };
  const id = evId();
  const ev = { id, source: "lucina", sourceId: id.slice(7), calendar: LUCINA_CAL, title, recurring: false, editable: true, addedBy, ...times };
  textFields(body, ev);
  own.push(ev);
  return { event: ev };
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
        id: "tasks",
        cards: [
          {
            id: "todoist",
            kind: "todoist",
            name: "Todoist",
            status: todoist ? "connected" : "disconnected",
            account: todoist ? todoist.account : null,
            oauth: false,
            method: todoist ? "token" : undefined,
            scopes: ["data:read_write"],
            lastSyncAt: todoist ? ago(40_000) : null,
            count: todoist ? 14 : undefined,
            actions: todoist ? ["sync", "disconnect"] : ["token"],
          },
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
  if (path === "/v1/links/todoist/token" && method === "POST") {
    const body = await readJson(req);
    const token = String(body.token ?? "").trim();
    if (token.length < 20 || token.startsWith("bad")) return fail(res, 422, "todoist_token_rejected", "Todoist did not accept that token"), true;
    todoist = { account: "owner@example.com" };
    return send(res, 200, { connected: true, account: todoist.account }), true;
  }
  if (path === "/v1/links/todoist/connect" && method === "POST") {
    return fail(res, 503, "todoist_oauth_not_configured", "Todoist sign-in is not configured on the server; paste the API token instead"), true;
  }
  if (path === "/v1/links/todoist/sync" && method === "POST") return send(res, 200, { ok: true }), true;
  if (path === "/v1/links/todoist" && method === "DELETE") {
    todoist = null;
    return send(res, 200, { disconnected: true, revokedAtTodoist: false }), true;
  }
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
    const from = parseBound(url.searchParams.get("from"), false) ?? dayAt(0, 0);
    const days = Number(url.searchParams.get("days") ?? 7);
    const to = parseBound(url.searchParams.get("to"), true) ?? from + days * DAY;
    const list = events().filter((e) => e.endMs > from && e.startMs < to);
    return send(res, 200, {
      from: iso(from),
      to: iso(to),
      timeZone: "Europe/Copenhagen",
      events: list,
      sources: {
        lucina: { count: own.length },
        google: googleConnected ? { status: googleReconnect ? "reconnect" : "connected", account: "owner@example.com", lastSyncAt: lastSync } : null,
        ics: ics.map((x) => ({ id: x.id, label: x.label, lastSyncAt: x.synced })),
        iphone: { device: "Giuseppe - iPhone", reportedAt: ago(5 * 60_000) },
      },
    }), true;
  }
  if (path === "/v1/calendar/events" && method === "POST") {
    const body = await readJson(req);
    const r = createOwn(body ?? {}, "Web");
    if (r.error) return fail(res, 400, "bad_request", r.error), true;
    return send(res, 201, { created: true, event: r.event }), true;
  }
  const evMatch = path.match(/^\/v1\/calendar\/events\/([^/]+)$/);
  if (evMatch && (method === "PATCH" || method === "DELETE")) {
    const id = decodeURIComponent(evMatch[1]);
    if (/^(iphone|ics):/.test(id)) {
      return fail(res, 422, "read_only_source", "only Lucina's and Google Calendar events can be changed here (ICS feeds are read-only; iPhone events change on the phone)"), true;
    }
    const store = id.startsWith("google:") ? google : own;
    const i = store.findIndex((e) => e.id === id);
    if (i < 0) return fail(res, 404, "event_not_found", "no such event"), true;
    if (method === "DELETE") {
      if (!["true", "1"].includes(url.searchParams.get("confirm") ?? "")) return fail(res, 428, "confirmation_required", "ask the owner first, then send confirm: true"), true;
      store.splice(i, 1);
      return send(res, 200, { deleted: true, id }), true;
    }
    const body = (await readJson(req)) ?? {};
    if (body.confirm !== true) return fail(res, 428, "confirmation_required", "ask the owner first, then send confirm: true"), true;
    const ev = { ...store[i] };
    if (typeof body.title === "string") {
      if (!body.title.trim()) return fail(res, 400, "bad_request", "title: required"), true;
      ev.title = body.title.trim();
    }
    if ("start" in body || "end" in body || "allDay" in body) {
      const times = timesFrom(body, store[i]);
      if (!times) return fail(res, 400, "bad_request", "start/end: a valid range"), true;
      Object.assign(ev, times);
    }
    const bad = textFields(body, ev);
    if (bad) return fail(res, 400, "bad_request", bad), true;
    store[i] = ev;
    return send(res, 200, { updated: true, event: ev }), true;
  }
  const moveMatch = path.match(/^\/v1\/calendar\/events\/([^/]+)\/move$/);
  if (moveMatch && method === "POST") {
    const id = decodeURIComponent(moveMatch[1]);
    const body = (await readJson(req)) ?? {};
    if (body.confirm !== true) return fail(res, 428, "confirmation_required", "ask the owner first, then send confirm: true"), true;
    const from = id.startsWith("google:") ? google : id.startsWith("lucina:") ? own : null;
    const to = body.target === "google" ? google : body.target === "lucina" ? own : null;
    if (!from || !to) return fail(res, 400, "bad_request", "target: lucina or google"), true;
    if (from === to) return fail(res, 400, "bad_request", "the event is already in that calendar"), true;
    const i = from.findIndex((e) => e.id === id);
    if (i < 0) return fail(res, 404, "event_not_found", "no such event"), true;
    if (from[i].recurring) return fail(res, 422, "recurring_event", "a repeating event can't be moved to another calendar here"), true;
    const [ev] = from.splice(i, 1);
    const nid = body.target === "google" ? `google:${Math.random().toString(36).slice(2, 10)}` : evId();
    const moved = { ...ev, id: nid, sourceId: nid.replace(/^[a-z]+:/, ""), source: body.target, calendar: body.target === "google" ? "Google" : LUCINA_CAL };
    to.push(moved);
    return send(res, 200, { moved: true, from: id, event: moved }), true;
  }
  if (path === "/v1/calendar/proposals" && method === "GET") {
    return send(res, 200, { proposals: proposals.filter((p) => p.expiresAtMs > Date.now()) }), true;
  }
  const propMatch = path.match(/^\/v1\/calendar\/proposals\/([^/]+)(\/accept)?$/);
  if (propMatch && ((method === "POST" && propMatch[2]) || (method === "DELETE" && !propMatch[2]))) {
    const id = decodeURIComponent(propMatch[1]);
    const p = proposals.find((x) => x.id === id);
    if (!p) return fail(res, 404, "proposal_not_found", "no such proposal"), true;
    if (method === "POST") {
      const body = (await readJson(req)) ?? {};
      if (body.confirm !== true) return fail(res, 428, "confirmation_required", "ask the owner first, then send confirm: true"), true;
      const r = createOwn(p.event, p.from);
      if (r.error) return fail(res, 400, "bad_request", r.error), true;
      proposals = proposals.filter((x) => x.id !== id);
      return send(res, 201, { created: true, event: r.event }), true;
    }
    proposals = proposals.filter((x) => x.id !== id);
    return send(res, 200, { dismissed: true }), true;
  }
  return false;
}
