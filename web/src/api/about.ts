import { request } from "./client";

export type FactCategory =
  | "identity" | "education" | "work" | "money" | "health" | "home"
  | "people" | "preferences" | "schedule" | "other";

export const FACT_CATEGORIES: FactCategory[] = [
  "identity", "education", "work", "money", "health", "home",
  "people", "preferences", "schedule", "other",
];

export type FactSourceKind = "chat" | "email" | "document" | "calendar" | "owner-edit" | "seed";

export interface Fact {
  id: string;
  text: string;
  category: string;
  source: { kind: FactSourceKind; reference?: string };
  learnedAt: string;
  lastConfirmed: string;
  validUntil?: { date?: string; condition?: string };
  status: "active" | "expired" | "retracted";
  endedAt?: string;
  changes: { at: string; by: string; action: string; note?: string; before?: string }[];
}

export interface AboutData {
  facts: Fact[];
  history: Fact[];
}

export interface FactInput {
  text: string;
  category: string;
  validUntil?: string;
  validUntilCondition?: string;
}

export interface FactPatch {
  text?: string;
  category?: string;
  validUntil?: string;
  validUntilCondition?: string;
}

export const aboutApi = {
  get: () => request<AboutData>("GET", "/v1/about"),
  add: (body: FactInput) => request<Fact>("POST", "/v1/about", { body }),
  update: (id: string, body: FactPatch) => request<Fact>("PATCH", `/v1/about/${encodeURIComponent(id)}`, { body }),
  end: (id: string, reason?: string) => request<Fact>("POST", `/v1/about/${encodeURIComponent(id)}/end`, { body: reason ? { reason } : {} }),
  retract: (id: string, reason?: string) => request<Fact>("POST", `/v1/about/${encodeURIComponent(id)}/retract`, { body: reason ? { reason } : {} }),
};
