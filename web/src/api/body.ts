import { request } from "./client";
import type { WeightUnit } from "../lib/units";

// Body-weight module wire types, per docs/server-api.md ("Body module").
// All body wire knowledge lives in this file. Weights are integer grams;
// the engine owns text parsing (`weight` strings, `parse_weight`) and every
// stat and label.
//
// GET  /v1/body/state?timezone=…  -> engine response {ok, view}
// POST /v1/body/command           -> {op_id, expected_revision, timezone, command}
//                                    200 ok · 409 conflict · 422 invalid_command/op_id_reused · 500 storage_error

export interface BodySettings {
  unit: WeightUnit;
  goal_grams: number | null;
}

export interface BodyEntry {
  id: string;
  weight_grams: number;
  measured_ms: number;
  note: string;
  created_ms?: number;
  updated_ms?: number;
}

export interface BodyStats {
  count: number;
  current_grams: number | null;
  current_measured_ms: number | null;
  avg7_grams: number | null;
  weekly_change_grams: number | null;
  min_grams: number | null;
  max_grams: number | null;
  trend30_grams: number | null;
  trend90_grams: number | null;
  goal_grams: number | null;
  goal_distance_grams: number | null;
  goal_reached: boolean | null;
}

/** Oldest first, with the 7-day average at each entry. */
export interface BodySeriesPoint {
  entry_id: string;
  measured_ms: number;
  weight_grams: number;
  avg7_grams: number;
}

/** Keys present when the stat is non-null. */
export type BodyStatLabel =
  | "current"
  | "avg7"
  | "weekly_change"
  | "min"
  | "max"
  | "trend30"
  | "trend90"
  | "goal"
  | "goal_distance";

export interface BodyView {
  revision: number;
  settings: BodySettings;
  /** Newest first (by measured_ms). */
  entries: BodyEntry[];
  stats: BodyStats;
  series: BodySeriesPoint[];
  /** entry id -> "80.5 kg" in the display unit. */
  labels: Record<string, string>;
  stat_labels: Partial<Record<BodyStatLabel, string>>;
}

export interface BodyResponse {
  ok: boolean;
  replayed?: boolean;
  code?: string;
  error?: string;
  view?: BodyView;
  /** Answer of the `parse_weight` query. */
  weight_grams?: number;
}

export interface BodyEnvelope {
  op_id: string;
  expected_revision: number;
  timezone: string;
  command: BodyCommand;
}

/** A weight is sent either as integer grams or as text in the display unit, never both. */
type WeightField = { weight_grams: number; weight?: never } | { weight: string; weight_grams?: never };

export type BodyCommand =
  | ({ type: "log_entry"; measured_ms?: number; note?: string } & WeightField)
  | ({ type: "edit_entry"; entry_id: string; measured_ms?: number; note?: string } & Partial<WeightField>)
  | { type: "delete_entry"; entry_id: string }
  | { type: "update_settings"; unit: WeightUnit; goal?: string; goal_grams?: number | null }
  | { type: "parse_weight"; value: string; unit?: WeightUnit };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

/** Accept the engine response `{ok, view}` or a bare view; fill optional maps. */
export function normalizeBodyView(raw: unknown): BodyView {
  const view = isRecord(raw) && isRecord(raw.view) ? raw.view : raw;
  if (!isRecord(view) || !isRecord(view.settings) || !Array.isArray(view.entries) || !isRecord(view.stats)) {
    throw new Error("The body server returned an unexpected response.");
  }
  return {
    ...(view as unknown as BodyView),
    series: Array.isArray(view.series) ? (view.series as BodySeriesPoint[]) : [],
    labels: isRecord(view.labels) ? (view.labels as Record<string, string>) : {},
    stat_labels: isRecord(view.stat_labels) ? (view.stat_labels as BodyView["stat_labels"]) : {},
  };
}

function tzQuery(timezone: string): string {
  return `?timezone=${encodeURIComponent(timezone)}`;
}

export async function getBodyState(timezone: string): Promise<BodyView> {
  return normalizeBodyView(await request<unknown>("GET", `/v1/body/state${tzQuery(timezone)}`));
}

export async function sendBodyCommand(envelope: BodyEnvelope): Promise<BodyResponse> {
  const raw = await request<BodyResponse & { view?: unknown }>("POST", "/v1/body/command", { body: envelope });
  if (raw.view === undefined) return raw;
  return { ...raw, view: normalizeBodyView(raw.view) };
}

/** `parse_weight` query: the engine turns text in `unit` into grams (throws ApiError on 422). */
export async function parseWeightOnServer(value: string, unit: WeightUnit, timezone: string): Promise<number> {
  const res = await request<BodyResponse>("POST", "/v1/body/command", {
    body: { timezone, command: { type: "parse_weight", value, unit } },
  });
  if (!res.ok || typeof res.weight_grams !== "number") throw new Error(res.error ?? "Enter a valid weight.");
  return res.weight_grams;
}
