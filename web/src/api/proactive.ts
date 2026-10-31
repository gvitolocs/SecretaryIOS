// Lucina's proactive outreach: feedback on a "Lucina reached out" message
// (POST /v1/proactive/outreach/{id}/feedback). The engine pulls the taps:
// "Non ora" holds that kind of message for a few hours, "Basta così" mutes it.

import { request } from "./client";
import { OUTREACH_ID, type FeedbackAction } from "./timeline";

export type OutreachAction = FeedbackAction | "opened";

export interface FeedbackTap {
  id: string;
  outreachId: string;
  action: OutreachAction;
  at: number;
}

export async function sendOutreachFeedback(outreachId: string, action: OutreachAction): Promise<FeedbackTap> {
  if (!OUTREACH_ID.test(outreachId)) throw new Error("not an outreach id");
  const r = await request<{ feedback: FeedbackTap }>(
    "POST",
    `/v1/proactive/outreach/${encodeURIComponent(outreachId)}/feedback`,
    { body: { action } },
  );
  return r.feedback;
}

/** The `outreach` deep-link parameter (`/?outreach=po_…`), if valid. */
export function outreachFromSearch(search: string): string | null {
  const v = new URLSearchParams(search).get("outreach");
  return v && OUTREACH_ID.test(v) ? v : null;
}
