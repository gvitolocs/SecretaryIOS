import { describe, expect, it } from "vitest";
import { linkify } from "./linkify";

describe("linkify", () => {
  it("returns plain text unchanged", () => {
    expect(linkify("hello\nworld")).toEqual([{ kind: "text", text: "hello\nworld" }]);
  });

  it("links http(s) and www URLs and trims trailing punctuation", () => {
    const segs = linkify("See https://example.com/a?b=1. Or www.example.org, ok");
    expect(segs).toEqual([
      { kind: "text", text: "See " },
      { kind: "link", text: "https://example.com/a?b=1", href: "https://example.com/a?b=1" },
      { kind: "text", text: ". Or " },
      { kind: "link", text: "www.example.org", href: "https://www.example.org/" },
      { kind: "text", text: ", ok" },
    ]);
  });

  it("keeps balanced parentheses and drops an unbalanced closer", () => {
    const wiki = linkify("https://en.wikipedia.org/wiki/Lucina_(Fire_Emblem)");
    expect(wiki[0]).toMatchObject({ kind: "link", text: "https://en.wikipedia.org/wiki/Lucina_(Fire_Emblem)" });
    const wrapped = linkify("(https://example.com/x)");
    expect(wrapped).toEqual([
      { kind: "text", text: "(" },
      { kind: "link", text: "https://example.com/x", href: "https://example.com/x" },
      { kind: "text", text: ")" },
    ]);
  });

  it("never links other schemes or markup", () => {
    const segs = linkify('javascript:alert(1) <a href="x">x</a> data:text/html,hi');
    expect(segs.every((s) => s.kind === "text")).toBe(true);
  });

  it("does not link the tail of a word", () => {
    expect(linkify("foowww.example.com").every((s) => s.kind === "text")).toBe(true);
  });
});
