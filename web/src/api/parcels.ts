import { request } from "./client";

// Parcels wire types. secretary-server forwards /v1/parcels… to lucina-rs /api/secretary/parcels…
// and passes Lucina's JSON through unchanged (camelCase, RFC 3339 UTC times).
//
// GET    /v1/parcels                -> {parcels, digestHour}
// POST   /v1/parcels {number, carrier?, label?} -> {parcel}: detects the carrier, saves, tracks once
// GET    /v1/parcels/{id}           -> {parcel}                     · 404 not_found
// POST   /v1/parcels/{id}/refresh   -> {parcel} (the stored one when checked under 10 minutes ago)
// PATCH  /v1/parcels/{id} {label}   -> {parcel}                     · label ≤ 80 characters, null clears it
// DELETE /v1/parcels/{id}           -> {ok}                         · 404 not_found
// 400 bad_request: a bad number · 503 parcels_unavailable: Lucina unreachable or the feature off.

export type ParcelState = "active" | "delivered" | "stopped";
export type ParcelSchedule = "hourly" | "daily" | "stopped";

/** The scraper's stages; a stage this client does not know arrives as a plain string. */
export const KNOWN_STAGES = [
  "pending",
  "registered",
  "accepted",
  "in_transit",
  "out_for_delivery",
  "delivered",
  "customs",
  "failed_attempt",
  "ready_for_pickup",
  "returned",
  "exception",
  "unknown",
] as const;

export type KnownStage = (typeof KNOWN_STAGES)[number];

export interface ParcelStatus {
  stage: KnownStage | string;
  text: string | null;
  lastUpdate: string | null;
  /** A date, `2026-09-28`. */
  expectedDelivery: string | null;
  location: string | null;
  destinationCountry: string | null;
}

export interface ParcelEvent {
  time: string | null;
  description: string;
  location: string | null;
  stage: KnownStage | string;
}

export interface Parcel {
  id: string;
  number: string;
  /** The scraper's carrier id. */
  carrier: string | null;
  label: string | null;
  state: ParcelState;
  schedule: ParcelSchedule;
  /** Near its destination: the reason for "hourly". */
  near: boolean;
  addedAt: string;
  lastChecked: string | null;
  nextCheck: string | null;
  lastError: string | null;
  status: ParcelStatus | null;
  /** Newest first, at most 50. */
  events: ParcelEvent[];
}

export interface ParcelList {
  parcels: Parcel[];
  digestHour: number | null;
}

export interface NewParcel {
  number: string;
  carrier?: string;
  label?: string;
}

const str = (v: unknown): string | null => (typeof v === "string" && v ? v : null);

function parseStatus(raw: unknown): ParcelStatus | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  return {
    stage: str(r.stage) ?? "unknown",
    text: str(r.text),
    lastUpdate: str(r.lastUpdate),
    expectedDelivery: str(r.expectedDelivery),
    location: str(r.location),
    destinationCountry: str(r.destinationCountry),
  };
}

function parseEvent(raw: unknown): ParcelEvent | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  return {
    time: str(r.time),
    description: typeof r.description === "string" ? r.description : "",
    location: str(r.location),
    stage: str(r.stage) ?? "unknown",
  };
}

/** One parcel from Lucina's JSON; missing optional fields and unknown strings do not throw. null: not a parcel. */
export function parseParcel(raw: unknown): Parcel | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const id = str(r.id);
  if (!id) return null;
  const state: ParcelState = r.state === "delivered" || r.state === "stopped" ? r.state : "active";
  const schedule: ParcelSchedule =
    r.schedule === "hourly" || r.schedule === "stopped" ? r.schedule : state === "active" ? "daily" : "stopped";
  return {
    id,
    number: typeof r.number === "string" ? r.number : "",
    carrier: str(r.carrier),
    label: str(r.label),
    state,
    schedule,
    near: r.near === true,
    addedAt: str(r.addedAt) ?? "",
    lastChecked: str(r.lastChecked),
    nextCheck: str(r.nextCheck),
    lastError: str(r.lastError),
    status: parseStatus(r.status),
    events: Array.isArray(r.events) ? r.events.map(parseEvent).filter((e): e is ParcelEvent => e !== null) : [],
  };
}

export function parseParcelList(raw: unknown): ParcelList {
  const r = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
  const parcels = Array.isArray(r.parcels) ? r.parcels.map(parseParcel).filter((p): p is Parcel => p !== null) : [];
  return { parcels, digestHour: typeof r.digestHour === "number" ? r.digestHour : null };
}

function one(raw: unknown): Parcel {
  const parcel = parseParcel(raw && typeof raw === "object" ? (raw as { parcel?: unknown }).parcel : null);
  if (!parcel) throw new Error("Lucina sent a parcel this app cannot read.");
  return parcel;
}

const path = (id: string) => `/v1/parcels/${encodeURIComponent(id)}`;

export const listParcels = async (): Promise<ParcelList> => parseParcelList(await request<unknown>("GET", "/v1/parcels"));

// Adding and checking ask the carrier, which can take a while.
export const addParcel = async (parcel: NewParcel): Promise<Parcel> => one(await request<unknown>("POST", "/v1/parcels", { body: parcel, timeoutMs: 45_000 }));

export const getParcel = async (id: string): Promise<Parcel> => one(await request<unknown>("GET", path(id)));

export const refreshParcel = async (id: string): Promise<Parcel> =>
  one(await request<unknown>("POST", `${path(id)}/refresh`, { body: {}, timeoutMs: 45_000 }));

export const setParcelLabel = async (id: string, label: string | null): Promise<Parcel> =>
  one(await request<unknown>("PATCH", path(id), { body: { label } }));

export const deleteParcel = (id: string) => request<{ ok: boolean }>("DELETE", path(id));
