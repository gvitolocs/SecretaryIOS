import { describe, expect, it } from "vitest";
import { askerLabel, decisionSummary, tidyDecisionText } from "./decisionText";

describe("decision questions in the chat", () => {
  it("drops the asker's session ids", () => {
    expect(askerLabel("claude-code (lucinamaps-courier (local_8721b9…))")).toBe("claude-code · lucinamaps-courier");
    expect(askerLabel("codex")).toBe("codex");
    expect(askerLabel("cursor (0123456789abcdef)")).toBe("cursor");
  });

  it("puts a short heading over the question and keeps the options", () => {
    const text =
      'claude-code (lucinamaps-courier (local_8721b9…)) needs a decision: Lucina Maps: which engine?\n\n1. Google\n2. OSM\n\nimpact medium · reason\nTap an option (or open Secretary → Decisions), or answer "Decision dc-1a2b: <your words>".';
    const out = tidyDecisionText(text);
    expect(out.startsWith("**Decision for you** · claude-code · lucinamaps-courier\n\nLucina Maps: which engine?")).toBe(true);
    expect(out).toContain("1. Google\n2. OSM");
    expect(out).toContain("“Decision dc-1a2b: …”");
    expect(out).not.toContain("local_");
    expect(out).not.toContain("<your words>");
  });

  it("summarises a decision for a notification in one line", () => {
    expect(decisionSummary("codex (fix-build (local_ab12cd34)) needs a decision: Ship it?\n\n1. Yes")).toBe("codex · fix-build: Ship it?\n\n1. Yes");
    expect(decisionSummary("Call the dentist")).toBe("Call the dentist");
  });

  it("leaves every other message alone", () => {
    expect(tidyDecisionText("All quiet. Details: https://example.com/status")).toBe("All quiet. Details: https://example.com/status");
  });
});
