import { describe, expect, it } from "vitest";
import { draftHasContent, draftToInput, emptyDraft, type Draft } from "./keepDraft";

const draft = (over: Partial<Draft>): Draft => ({ ...emptyDraft(), ...over });
const item = (text: string, checked = false) => ({ id: `i_${text}`, text, checked, indent: 0 as const });
const file = (name: string) => new File(["x"], name, { type: "image/png" });

describe("the composer's save rule", () => {
  it("discards an untouched note", () => {
    expect(draftHasContent(emptyDraft())).toBe(false);
    expect(draftToInput(emptyDraft())).toBeNull();
  });

  it("discards whitespace-only text and empty list rows", () => {
    expect(draftToInput(draft({ title: "  ", content: " \n " }))).toBeNull();
    expect(draftToInput(draft({ items: [item(""), item("  ")] }))).toBeNull();
  });

  it("does not count colour, pin, labels, reminder or collaborators on their own", () => {
    expect(draftToInput(draft({ color: "red", pinned: true, labels: ["Chill"], reminder: { at: "2026-10-30T08:00:00.000Z" }, collaborators: ["a@b.co"] }))).toBeNull();
  });

  it("saves a title alone, text alone, or list items alone", () => {
    expect(draftToInput(draft({ title: "Plan" }))).toMatchObject({ title: "Plan" });
    expect(draftToInput(draft({ content: "buy milk" }))).toMatchObject({ title: "", content: "buy milk" });
    expect(draftToInput(draft({ items: [item("eggs"), item("")] }))?.items).toEqual([item("eggs")]);
  });

  it("carries the options with the note", () => {
    const input = draftToInput(draft({ title: "T", color: "teal", pinned: true, archived: true, labels: ["GodLevel"], collaborators: ["a@b.co"], reminder: { at: "2026-10-30T08:00:00.000Z", recurrence: "weekly" } }));
    expect(input).toEqual({
      title: "T",
      content: "",
      color: "teal",
      pinned: true,
      archived: true,
      labels: ["GodLevel"],
      collaborators: ["a@b.co"],
      reminder: { at: "2026-10-30T08:00:00.000Z", recurrence: "weekly" },
    });
  });

  it("saves an image-only note, titled after the file because the server wants some text", () => {
    expect(draftHasContent(draft({ files: [file("holiday.jpg")] }))).toBe(true);
    expect(draftToInput(draft({ files: [file("holiday.jpg")] }))?.title).toBe("holiday");
    expect(draftToInput(draft({ content: "caption", files: [file("a.png")] }))?.title).toBe("");
  });

  it("trims trailing whitespace of the text", () => {
    expect(draftToInput(draft({ content: "hello  \n\n" }))?.content).toBe("hello");
  });
});
