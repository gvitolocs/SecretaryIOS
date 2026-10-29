import { request } from "./client";

// Links hub and calendar wire types, per docs/server-api.md ("Links" and
// "Calendar"). The server computes every status; this file only moves JSON.
//
// GET    /v1/links                      -> LinksOverview
// POST   /v1/links/google/connect       -> {authorizeUrl}
// DELETE /v1/links/google               -> {disconnected, revokedAtGoogle}
// POST   /v1/links/ics {label?, url}    -> 201 {id, label, host, eventCount, feedName}
// DELETE /v1/links/ics/{id}
// POST   /v1/links/custom {kind?, label, url, icon?, tags?}
// PATCH  /v1/links/custom/{id} · DELETE /v1/links/custom/{id}
// POST   /v1/calendar/sync
// POST   /v1/links/todoist/token {token} · POST /v1/links/todoist/connect -> {authorizeUrl}
// POST   /v1/links/todoist/sync · DELETE /v1/links/todoist
// GET    /v1/calendar/events?from&to&days
// POST   /v1/calendar/proposals/{id}/accept {confirm: true} · DELETE /v1/calendar/proposals/{id}

export type LinkStatus =
  | "connected"
  | "disconnected"
  | "reconnect"
  | "error"
  | "planned"
  | "available"
  | "unavailable"
  | "unknown"
  | "saved";

export type LinkAction = "connect" | "disconnect" | "reconnect" | "sync" | "remove" | "edit" | "token";

export interface LinkCard {
  id: string;
  kind: "google" | "iphone" | "ics" | "channel" | "ai" | "bookmark" | "mcp" | string;
  name: string;
  status: LinkStatus | string;
  service?: "calendar" | "gmail" | "drive";
  account?: string | null;
  scopes?: string[];
  lastSyncAt?: string | null;
  detail?: string | null;
  error?: string | null;
  count?: number;
  url?: string | null;
  icon?: string | null;
  tags?: string[];
  actions: LinkAction[];
  /** Todoist: an OAuth app is configured (else only the personal token). */
  oauth?: boolean;
  /** Todoist: `token` or `oauth`. */
  method?: string;
}

export type GroupId = "calendars" | "tasks" | "channels" | "ai" | "google" | "custom";

export interface LinkGroup {
  id: GroupId | string;
  cards: LinkCard[];
}

export interface ProposedEvent {
  title: string;
  start: string;
  end?: string;
  allDay?: boolean;
  location?: string;
  description?: string;
}

export interface Proposal {
  id: string;
  event: ProposedEvent;
  from: string;
  /** Why it is a proposal: Lucina's direct create for the owner failed. */
  reason?: string;
  createdAt: string;
  expiresAtMs: number;
}

export interface LinksOverview {
  timeZone: string;
  google: { configured: boolean; status: string; account: string | null; services: string[] };
  groups: LinkGroup[];
  proposals: Proposal[];
}

export interface CalendarEvent {
  id: string;
  source: "google" | "ics" | "iphone";
  sourceId: string;
  calendar?: string;
  title: string;
  start: string;
  end: string;
  allDay: boolean;
  startMs: number;
  endMs: number;
  location?: string;
  description?: string;
  url?: string;
  recurring: boolean;
  editable: boolean;
}

export interface EventsPage {
  from: string;
  to: string;
  timeZone: string;
  events: CalendarEvent[];
}

export interface NewLink {
  kind?: "bookmark" | "mcp";
  label: string;
  url: string;
  icon?: string;
  tags?: string[];
}

export const getLinks = () => request<LinksOverview>("GET", "/v1/links");

export const connectGoogle = (services: string[]) =>
  request<{ authorizeUrl: string }>("POST", "/v1/links/google/connect", { body: { services, client: "web" } });

export const disconnectGoogle = () =>
  request<{ disconnected: boolean; revokedAtGoogle: boolean }>("DELETE", "/v1/links/google");

export const addIcs = (label: string, url: string) =>
  request<{ id: string; label: string; host: string; eventCount: number; feedName: string | null }>("POST", "/v1/links/ics", {
    body: { label: label || undefined, url },
    timeoutMs: 30_000,
  });

export const removeIcs = (id: string) => request<{ removed: boolean }>("DELETE", `/v1/links/ics/${encodeURIComponent(id)}`);

export const addCustom = (link: NewLink) => request<unknown>("POST", "/v1/links/custom", { body: link });

export const removeCustom = (id: string) =>
  request<{ removed: boolean }>("DELETE", `/v1/links/custom/${encodeURIComponent(id)}`);

export const syncCalendars = () => request<unknown>("POST", "/v1/calendar/sync", { body: {}, timeoutMs: 60_000 });

export const getEvents = (days: number) =>
  request<EventsPage>("GET", `/v1/calendar/events?days=${encodeURIComponent(String(days))}`);

export const acceptProposal = (id: string) =>
  request<unknown>("POST", `/v1/calendar/proposals/${encodeURIComponent(id)}/accept`, { body: { confirm: true } });

export const dismissProposal = (id: string) =>
  request<unknown>("DELETE", `/v1/calendar/proposals/${encodeURIComponent(id)}`);

export const saveTodoistToken = (token: string) =>
  request<{ connected: boolean; account: string | null }>("POST", "/v1/links/todoist/token", { body: { token }, timeoutMs: 30_000 });

export const connectTodoist = () =>
  request<{ authorizeUrl: string }>("POST", "/v1/links/todoist/connect", { body: { client: "web" } });

export const syncTodoist = () => request<unknown>("POST", "/v1/links/todoist/sync", { body: {}, timeoutMs: 45_000 });

export const disconnectTodoist = () =>
  request<{ disconnected: boolean; revokedAtTodoist: boolean }>("DELETE", "/v1/links/todoist");
