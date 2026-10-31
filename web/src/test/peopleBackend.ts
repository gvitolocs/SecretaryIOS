import type { Call } from "./fetch";
import { stubFetch, type Handler } from "./fetch";

/** A synthetic people backend for tests: the face settings, the people, the faces without a person, and what was asked. */
export interface RawPerson {
  id: string;
  name: string | null;
  hidden: boolean;
  birth_date: string | null;
  face_count: number;
  item_count: number;
  cover_face_id: string | null;
  thumb_url: string;
}

export const rawPerson = (id: string, over: Partial<RawPerson> = {}): RawPerson => ({
  id,
  name: null,
  hidden: false,
  birth_date: null,
  face_count: 5,
  item_count: 4,
  cover_face_id: `${id}_face`,
  thumb_url: `/v1/media/people/${id}/thumb?v=1`,
  ...over,
});

export interface PeopleBackend {
  calls: Call[];
  settings: Record<string, unknown>;
  people: RawPerson[];
  unassigned: { id: string; item_id: string; thumb_url: string; score: number }[];
  /** Make the next request to this "METHOD path" fail with this status. */
  failNext: Map<string, number>;
  /** Items of each person for `people/{id}/items`, by id, newest first. */
  items: Map<string, Record<string, unknown>[]>;
  pageSize: number;
  /** Answers first, for the routes a test adds (smart search…); undefined passes the request on. */
  extra?: (call: Call, url: URL) => ReturnType<Handler> | undefined;
}

export function peopleBackend(init: Partial<Pick<PeopleBackend, "settings" | "people" | "unassigned" | "items" | "pageSize" | "extra">> = {}): PeopleBackend {
  const b: PeopleBackend = {
    calls: [],
    settings: { enabled: true, model: "m", scanned: 100, pending: 0, faces: 40, people: 3, running: false, paused_hot: false, paused_busy: false, ...init.settings },
    people: init.people ?? [],
    unassigned: init.unassigned ?? [],
    failNext: new Map(),
    items: init.items ?? new Map(),
    pageSize: init.pageSize ?? 2,
    extra: init.extra,
  };
  b.calls = stubFetch((call) => {
    const url = new URL(call.url, "http://x");
    const key = `${call.method} ${url.pathname}`;
    const added = b.extra?.(call, url);
    if (added !== undefined) return added;
    const status = b.failNext.get(key);
    if (status) {
      b.failNext.delete(key);
      return { status, body: { error: { code: "x", message: "x" } } };
    }
    if (key === "GET /v1/media/faces/settings") return { body: b.settings };
    if (key === "PUT /v1/media/faces/settings") {
      b.settings = { ...b.settings, enabled: (call.body as { enabled: boolean }).enabled };
      return { body: b.settings };
    }
    if (key === "DELETE /v1/media/faces") {
      b.people = [];
      b.unassigned = [];
      b.settings = { ...b.settings, enabled: false, faces: 0, people: 0 };
      return { body: { ok: true } };
    }
    if (key === "GET /v1/media/people") {
      const hidden = url.searchParams.get("include_hidden") === "true";
      return { body: { people: b.people.filter((p) => hidden || !p.hidden) } };
    }
    const one = /^\/v1\/media\/people\/([^/]+)$/.exec(url.pathname);
    if (one && call.method === "GET") {
      const p = b.people.find((x) => x.id === decodeURIComponent(one[1]!));
      return p ? { body: p } : { status: 404, body: { error: { code: "not_found", message: "no" } } };
    }
    if (one && call.method === "PATCH") {
      const p = b.people.find((x) => x.id === one[1]);
      if (!p) return { status: 404, body: { error: { code: "not_found", message: "no" } } };
      const body = call.body as Record<string, unknown>;
      if ("name" in body) p.name = body.name as string | null;
      if ("hidden" in body) p.hidden = body.hidden as boolean;
      if ("birth_date" in body) p.birth_date = body.birth_date as string | null;
      return { body: p };
    }
    const merge = /^\/v1\/media\/people\/([^/]+)\/merge$/.exec(url.pathname);
    if (merge && call.method === "POST") {
      const from = (call.body as { from: string[] }).from;
      const keep = b.people.find((x) => x.id === merge[1])!;
      keep.face_count += b.people.filter((x) => from.includes(x.id)).reduce((n, x) => n + x.face_count, 0);
      b.people = b.people.filter((x) => !from.includes(x.id));
      return { body: keep };
    }
    const items = /^\/v1\/media\/people\/([^/]+)\/items$/.exec(url.pathname);
    if (items) {
      const all = b.items.get(decodeURIComponent(items[1]!)) ?? [];
      const start = Number(url.searchParams.get("cursor") ?? 0);
      const page = all.slice(start, start + b.pageSize);
      return { body: { items: page, next_cursor: start + b.pageSize < all.length ? String(start + b.pageSize) : null, total: all.length } };
    }
    if (key === "GET /v1/media/faces/unassigned") {
      const start = Number(url.searchParams.get("cursor") ?? 0);
      const page = b.unassigned.slice(start, start + 2);
      return { body: { faces: page, next_cursor: start + 2 < b.unassigned.length ? String(start + 2) : null } };
    }
    const face = /^\/v1\/media\/faces\/([^/]+)$/.exec(url.pathname);
    if (face && call.method === "PUT") {
      b.unassigned = b.unassigned.filter((f) => f.id !== face[1]);
      const body = call.body as { person_id?: string | null; new_person?: string };
      const person = body.new_person ? { id: "p_new", name: body.new_person } : body.person_id ? { id: body.person_id, name: b.people.find((p) => p.id === body.person_id)?.name ?? null } : null;
      return { body: { id: face[1], box: { x: 0.1, y: 0.1, w: 0.2, h: 0.2 }, score: 0.9, source: "manual", person } };
    }
    return { status: 404, body: { error: { code: "not_found", message: `${key} not stubbed` } } };
  });
  return b;
}
