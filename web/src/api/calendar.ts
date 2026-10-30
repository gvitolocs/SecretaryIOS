import { request } from "./client";

// The Calendar tab's wire types and calls, per docs/server-api.md ("Calendar").
// Lucina's own calendar (`lucina:` ids) is the source of truth and always writable; Google events are writable when
// the server says so (`editable`); iPhone and ICS events are read-only.
//
// GET    /v1/calendar/events?from&to      -> EventsPage
// POST   /v1/calendar/events {title,start,end?,allDay?,location?,description?,conferenceUrl?} -> 201 {created, event}
// PATCH  /v1/calendar/events/{id} {…, confirm: true} -> {updated, event}
// DELETE /v1/calendar/events/{id}?confirm=true -> {deleted, id}
// GET    /v1/calendar/proposals -> {proposals}
// POST   /v1/calendar/proposals/{id}/accept {confirm: true} · DELETE /v1/calendar/proposals/{id}
// POST   /v1/calendar/sync

export type CalendarSource = "lucina" | "google" | "ics" | "iphone";

export interface CalEvent {
  id: string;
  source: CalendarSource | string;
  sourceId: string;
  calendar?: string;
  title: string;
  /** RFC 3339 for timed events; `YYYY-MM-DD` for all-day events. */
  start: string;
  /** Exclusive end, same format as `start`. */
  end: string;
  allDay: boolean;
  startMs: number;
  endMs: number;
  location?: string;
  description?: string;
  url?: string;
  recurring: boolean;
  editable: boolean;
  attendees?: string[];
  addedBy?: string;
  conferenceUrl?: string;
}

export interface CalSources {
  lucina?: { count?: number } | null;
  google?: { status?: string; account?: string | null } | null;
  ics?: { id: string; label: string }[];
  iphone?: { device?: string | null; reportedAt?: string | null } | null;
}

export interface CalEventsPage {
  from: string;
  to: string;
  timeZone: string;
  events: CalEvent[];
  sources?: CalSources;
}

export interface EventDraft {
  title: string;
  start: string;
  end?: string;
  allDay?: boolean;
  location?: string;
  description?: string;
  conferenceUrl?: string;
}

export interface ProposedEvent {
  title: string;
  start: string;
  end?: string;
  allDay?: boolean;
  location?: string;
  description?: string;
}

export interface CalProposal {
  id: string;
  event: ProposedEvent;
  from: string;
  createdAt: string;
  expiresAtMs: number;
  reason?: string;
}

const enc = encodeURIComponent;

export const listEvents = (from: Date, to: Date) =>
  request<CalEventsPage>("GET", `/v1/calendar/events?from=${enc(from.toISOString())}&to=${enc(to.toISOString())}`);

export const createEvent = (draft: EventDraft) =>
  request<{ created: boolean; event: CalEvent | null }>("POST", "/v1/calendar/events", { body: draft });

/** Pressing Save (or dropping a dragged event) is the owner's confirmation, so the edit carries `confirm: true`. */
export const updateEvent = (id: string, patch: Partial<EventDraft>) =>
  request<{ updated: boolean; event: CalEvent | null }>("PATCH", `/v1/calendar/events/${enc(id)}`, {
    body: { ...patch, confirm: true },
  });

export const deleteEvent = (id: string) =>
  request<{ deleted: boolean; id: string }>("DELETE", `/v1/calendar/events/${enc(id)}?confirm=true`);

export const listProposals = () => request<{ proposals: CalProposal[] }>("GET", "/v1/calendar/proposals");

export const acceptProposal = (id: string) =>
  request<{ created: boolean; event: CalEvent | null }>("POST", `/v1/calendar/proposals/${enc(id)}/accept`, {
    body: { confirm: true },
  });

export const dismissProposal = (id: string) => request<unknown>("DELETE", `/v1/calendar/proposals/${enc(id)}`);

export const syncCalendars = () => request<unknown>("POST", "/v1/calendar/sync", { body: {}, timeoutMs: 60_000 });
