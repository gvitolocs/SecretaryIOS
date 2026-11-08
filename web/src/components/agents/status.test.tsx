import { describe, expect, it } from "vitest";
import { statusLabel } from "./status";

// A run that hit a usage limit and continued on another agent (lucina-agentd handoff labels).
describe("statusLabel with hand-off labels", () => {
  it("says where a handed-on run continues", () => {
    expect(statusLabel({ bucket: "done", status: "error", labels: { "lucina.continued-on": "x", "lucina.continued-provider": "vibe" } })).toBe("Continued on vibe");
  });
  it("says a hand-off is being decided", () => {
    expect(statusLabel({ bucket: "running", status: "error", labels: { "lucina.handoff": "pending" } })).toBe("Handing on…");
  });
  it("leaves an exhausted or plain error an error", () => {
    expect(statusLabel({ bucket: "failed", status: "error", labels: { "lucina.handoff": "exhausted" } })).toBe("Error");
    expect(statusLabel({ bucket: "failed", status: "error" })).toBe("Error");
  });
});
