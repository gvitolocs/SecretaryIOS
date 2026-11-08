import { request } from "./client";
import { api } from "./secretary";
import { chatSessionId, uuid } from "../lib/ids";

export type DecisionStatus = "pending" | "decided" | "expired" | "withdrawn";
export interface Decision {
  decision_id: string;
  status: DecisionStatus;
  decision: "go" | "change" | "owner_reply" | null;
  chosen_option: string | null;
  chosen_index: number | null;
  reason: string | null;
  decided_by: "lucina" | "owner" | null;
  confidence: number | null;
  asked_owner: boolean;
  owner_only: boolean;
  question: string;
  options: string[];
  recommendation: string | null;
  impact: "low" | "medium" | "high";
  reversible: boolean;
  urgency: "low" | "normal" | "high";
  topic: string | null;
  asking_client: string;
  asking_session: string | null;
  created_at: string;
  decided_at: string | null;
  expires_at: string | null;
  basis: string | null;
  evidence: { kind: string; text: string; tier: number }[];
}

export const answerText = (id: string, n: number, option: string): string => `Decision ${id}: ${n}. ${option}`;
export const freeText = (id: string, text: string): string => `Decision ${id}: ${text}`;

/** Answers are ordinary web chat messages; there is no decision mutation endpoint. */
export function answer(id: string, response: string | { n: number; option: string }) {
  return api.sendChat({
    message: typeof response === "string" ? freeText(id, response) : answerText(id, response.n, response.option),
    sessionId: chatSessionId(),
    clientMessageId: uuid(),
  });
}

export const decisionsApi = {
  async list(status?: DecisionStatus, limit = 50): Promise<Decision[]> {
    const query = new URLSearchParams({ ...(status ? { status } : {}), limit: String(limit) });
    return (await request<{ items: Decision[] }>("GET", `/v1/decisions?${query}`)).items;
  },
  get: (id: string) => request<Decision>("GET", `/v1/decisions/${encodeURIComponent(id)}`),
  answer,
};
