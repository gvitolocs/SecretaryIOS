import { describe, expect, it } from "vitest";
import { domToMd, hostOf, linksIn, mdToDom, parseInline, parseKeepText, plainText, splitTitleHeading } from "./keepText";

const html = (s: string): HTMLElement => {
  const d = document.createElement("div");
  d.innerHTML = s;
  return d;
};
const roundTrip = (md: string): string => {
  const holder = document.createElement("div");
  holder.appendChild(mdToDom(md, document));
  return domToMd(holder);
};

describe("parseInline", () => {
  it("reads bold, italic, underline and links", () => {
    expect(parseInline("a **b** *c* <u>d</u> [e](https://x.org/p)")).toEqual([
      { t: "text", text: "a " },
      { t: "b", c: [{ t: "text", text: "b" }] },
      { t: "text", text: " " },
      { t: "i", c: [{ t: "text", text: "c" }] },
      { t: "text", text: " " },
      { t: "u", c: [{ t: "text", text: "d" }] },
      { t: "text", text: " " },
      { t: "a", href: "https://x.org/p", c: [{ t: "text", text: "e" }] },
    ]);
  });

  it("nests bold and italic", () => {
    expect(parseInline("**a *b* c**")).toEqual([
      {
        t: "b",
        c: [
          { t: "text", text: "a " },
          { t: "i", c: [{ t: "text", text: "b" }] },
          { t: "text", text: " c" },
        ],
      },
    ]);
  });

  it("keeps unmatched markers and spaced stars literal", () => {
    expect(parseInline("2 * 3 * 4 and **open")).toEqual([{ t: "text", text: "2 * 3 * 4 and **open" }]);
  });

  it("links bare URLs without the trailing punctuation, and refuses javascript: links", () => {
    const nodes = parseInline("see https://example.org/a_b, then [x](javascript:alert(1))");
    expect(nodes[1]).toEqual({ t: "a", href: "https://example.org/a_b", c: [{ t: "text", text: "https://example.org/a_b" }] });
    expect(JSON.stringify(nodes)).not.toContain('"href":"javascript');
  });
});

describe("parseKeepText", () => {
  it("makes a block per line: headings, bullets, tasks, blanks", () => {
    const blocks = parseKeepText("# Title\n## Small\n- one\n  - nested\n1. first\n- [x] done\n- [ ] todo\n\ntext");
    expect(blocks.map((b) => b.t)).toEqual(["h", "h", "li", "li", "li", "task", "task", "blank", "p"]);
    expect(blocks[0]).toMatchObject({ t: "h", level: 1 });
    expect(blocks[1]).toMatchObject({ t: "h", level: 2 });
    expect(blocks[3]).toMatchObject({ t: "li", indent: 1 });
    expect(blocks[4]).toMatchObject({ t: "li", marker: "1." });
    expect(blocks[5]).toMatchObject({ t: "task", checked: true });
  });

  it("drops blank lines at both ends only", () => {
    expect(parseKeepText("\n\na\n\nb\n\n").map((b) => b.t)).toEqual(["p", "blank", "p"]);
  });
});

describe("plain text and links", () => {
  it("strips the markers for search", () => {
    expect(plainText("# Hi **there** <u>you</u> [site](https://x.org) *ok*")).toBe("Hi there you site ok");
  });
  it("lists the links of a text once each", () => {
    expect(linksIn("https://a.org and [b](https://b.org) and https://a.org")).toEqual(["https://a.org", "https://b.org"]);
    expect(hostOf("https://www.example.org/x")).toBe("example.org");
  });
});

describe("the editor DOM and markdown", () => {
  it("round-trips the supported marks", () => {
    for (const md of ["plain", "a **b** c", "*i* and <u>u</u>", "# H1\n## H2\ntext", "line one\n\nline three", "see [x](https://x.org) now", "- a\n- b"]) {
      expect(roundTrip(md)).toBe(md);
    }
  });

  it("reads what a browser's contenteditable makes", () => {
    expect(domToMd(html("<div>one</div><div><br></div><div>two <b>bold</b> <i>it</i> <u>un</u></div>"))).toBe("one\n\ntwo **bold** *it* <u>un</u>");
    expect(domToMd(html("<h1>Head</h1><div>body</div><h2>Sub</h2>"))).toBe("# Head\nbody\n## Sub");
    expect(domToMd(html("first<div>second</div>"))).toBe("first\nsecond");
    expect(domToMd(html("<div>a<br>b</div>"))).toBe("a\nb");
  });

  it("keeps the spaces outside the marks and unwraps unsafe links", () => {
    expect(domToMd(html("<div><b>word </b>next</div>"))).toBe("**word** next");
    expect(domToMd(html('<div><a href="javascript:alert(1)">bad</a> <a href="https://ok.org">https://ok.org</a></div>'))).toBe("bad https://ok.org");
  });

  it("builds elements, never raw HTML", () => {
    const frag = mdToDom("**<img src=x onerror=alert(1)>**", document);
    const holder = document.createElement("div");
    holder.appendChild(frag);
    expect(holder.querySelector("img")).toBeNull();
    expect(holder.textContent).toBe("<img src=x onerror=alert(1)>");
  });
});

describe("splitTitleHeading", () => {
  it("splits off a leading # line that repeats the title, with the blank lines after it", () => {
    const md = "# CRAFT-IT4SD: meeting\n\nBriefing text\n## Questions";
    const { head, body } = splitTitleHeading(md, "CRAFT-IT4SD: meeting");
    expect(body).toBe("Briefing text\n## Questions");
    expect(head + body).toBe(md);
  });

  it("matches ## too, ignores case, emphasis and surrounding spaces", () => {
    expect(splitTitleHeading("## **Gift Ideas**\nbook", "  gift ideas ").body).toBe("book");
    expect(splitTitleHeading("# *Rome* <u>trip</u>\nx", "Rome trip").body).toBe("x");
    expect(splitTitleHeading("\n\n# Plan #\nx", "plan").body).toBe("x");
  });

  it("keeps a heading that differs from the title, a later heading, and notes without a title", () => {
    expect(splitTitleHeading("# Other\nx", "Plan")).toEqual({ head: "", body: "# Other\nx" });
    expect(splitTitleHeading("text\n# Plan", "Plan").head).toBe("");
    expect(splitTitleHeading("# Plan\nx", "").head).toBe("");
    expect(splitTitleHeading("### Plan\nx", "Plan").head).toBe("");
  });

  it("handles a note that is only the heading", () => {
    expect(splitTitleHeading("# Plan", "Plan")).toEqual({ head: "# Plan", body: "" });
  });
});

