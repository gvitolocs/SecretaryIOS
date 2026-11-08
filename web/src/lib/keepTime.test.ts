import { describe, expect, it } from "vitest";
import { editedLabel, fromInputs, reminderLabel, reminderPresets, toDateInput, toTimeInput } from "./keepTime";

const at = (iso: string) => new Date(iso);

describe("Keep's dates", () => {
  const now = at("2026-10-27T10:00:00");

  it("shows the time for today's edits and the date otherwise", () => {
    expect(editedLabel("2026-10-27T08:30:00", now)).toMatch(/8:30|08:30/);
    expect(editedLabel("2026-10-20T08:30:00", now)).toMatch(/Oct/);
    expect(editedLabel("2025-10-20T08:30:00", now)).toMatch(/2025/);
    expect(editedLabel("nope", now)).toBe("");
  });

  it("says Today and Tomorrow for reminders", () => {
    expect(reminderLabel("2026-10-27T18:00:00", now)).toMatch(/^Today, /);
    expect(reminderLabel("2026-10-28T08:00:00", now)).toMatch(/^Tomorrow, /);
    expect(reminderLabel("2026-11-02T08:00:00", now)).toMatch(/Nov/);
  });

  it("offers later today, tomorrow morning and next Monday", () => {
    const p = reminderPresets(now);
    expect(p.map((x) => x.id)).toEqual(["laterToday", "tomorrow", "nextWeek"]);
    expect(p[0]!.at.getHours()).toBe(18);
    expect(p[1]!.at.getDate()).toBe(28);
    expect(p[1]!.at.getHours()).toBe(8);
    expect(p[2]!.at.getDay()).toBe(1);
    expect(p[2]!.at.getTime()).toBeGreaterThan(now.getTime());
  });

  it("skips 'later today' late in the evening", () => {
    expect(reminderPresets(at("2026-10-27T22:30:00")).map((x) => x.id)).toEqual(["tomorrow", "nextWeek"]);
  });

  it("round-trips the date and time inputs", () => {
    const d = fromInputs("2026-10-30", "07:45")!;
    expect(toDateInput(d)).toBe("2026-10-30");
    expect(toTimeInput(d)).toBe("07:45");
    expect(fromInputs("", "07:45")).toBeNull();
  });
});
