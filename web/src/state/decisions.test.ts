import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  DECISIONS_POLL_MS, dismissDecision, pendingDecisionCount, refreshDecisions, resetDecisions, startDecisions, unannouncedDecisions,
} from "./decisions";
import { stubFetch } from "../test/fetch";

const item = (id: string) => ({ decision_id: id, status: "pending", question: `Q ${id}`, options: ["Go", "No"] });

describe("pending decisions store", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetDecisions();
  });
  afterEach(() => {
    resetDecisions();
    vi.useRealTimers();
  });

  it("polls every 30 s while visible, not while hidden, and refreshes when the tab comes back", async () => {
    let pending = [item("dc-1")];
    const calls = stubFetch(() => ({ body: { items: pending } }));
    const stop = startDecisions();
    await vi.advanceTimersByTimeAsync(0);
    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe("/v1/decisions?status=pending&limit=50");
    expect(pendingDecisionCount()).toBe(1);

    pending = [item("dc-1"), item("dc-2")];
    await vi.advanceTimersByTimeAsync(DECISIONS_POLL_MS);
    expect(calls).toHaveLength(2);
    expect(pendingDecisionCount()).toBe(2);

    const hidden = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    await vi.advanceTimersByTimeAsync(DECISIONS_POLL_MS * 2);
    expect(calls).toHaveLength(2);
    hidden.mockReturnValue("visible");
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.advanceTimersByTimeAsync(0);
    expect(calls).toHaveLength(3);
    hidden.mockRestore();

    stop();
    await vi.advanceTimersByTimeAsync(DECISIONS_POLL_MS * 2);
    expect(calls).toHaveLength(3);
  });

  it("keeps the last list when a refresh fails, and announces each id until dismissed", async () => {
    let fail = false;
    stubFetch(() => (fail ? { status: 500, body: {} } : { body: { items: [item("dc-1"), item("dc-2")] } }));
    await refreshDecisions();
    expect(unannouncedDecisions().map((d) => d.decision_id)).toEqual(["dc-1", "dc-2"]);
    dismissDecision("dc-1");
    fail = true;
    await refreshDecisions();
    expect(pendingDecisionCount()).toBe(2);
    expect(unannouncedDecisions().map((d) => d.decision_id)).toEqual(["dc-2"]);
  });
});
