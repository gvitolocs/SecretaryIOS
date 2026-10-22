import { describe, expect, it } from "vitest";
import { inlineText, parseInline, parseMarkdown, safeLink, type Block, type Inline } from "./markdown";

const links = (nodes: Inline[]): string[] =>
  nodes.flatMap((n) => (n.t === "link" ? [n.href, ...links(n.children)] : "children" in n ? links(n.children) : []));

describe("markdown", () => {
  it("parses headings, paragraphs, lists, quotes, code, rules and tables", () => {
    const blocks = parseMarkdown(
      [
        "# CRAFT-IT4SD",
        "",
        "Marianne offers **paid hours**",
        "and *flexible* times.",
        "",
        "## Questions",
        "1. Excel survey",
        "2. Resources",
        "   - CSV import",
        "   - admin panel",
        "",
        "- [x] read the brief",
        "- [ ] book the meeting",
        "",
        "> I worked on role-based assessments.",
        "",
        "```js",
        "const a = 1 < 2;",
        "```",
        "---",
        "| Stage | Range |",
        "|:--|--:|",
        "| Early | 0-30 |",
      ].join("\n"),
    );
    expect(blocks.map((b) => b.t)).toEqual(["heading", "paragraph", "heading", "list", "list", "quote", "code", "hr", "table"]);
    const p = blocks[1] as Block & { t: "paragraph" };
    expect(p.children.map((n) => n.t)).toEqual(["text", "strong", "br", "text", "em", "text"]);
    const ol = blocks[3] as Block & { t: "list" };
    expect(ol.ordered).toBe(true);
    expect(ol.items).toHaveLength(2);
    const nested = ol.items[1]!.children[1] as Block & { t: "list" };
    expect(nested.t).toBe("list");
    expect(nested.items).toHaveLength(2);
    const tasks = blocks[4] as Block & { t: "list" };
    expect(tasks.items.map((i) => i.checked)).toEqual([true, false]);
    expect((blocks[6] as Block & { t: "code" }).text).toBe("const a = 1 < 2;");
    const table = blocks[8] as Block & { t: "table" };
    expect(table.align).toEqual(["left", "right"]);
    expect(inlineText(table.rows[0]![1]!)).toBe("0-30");
  });

  it("never produces HTML and only safe links", () => {
    const blocks = parseMarkdown('<script>alert(1)</script> <img src=x onerror="alert(1)">');
    expect(blocks).toHaveLength(1);
    expect(inlineText((blocks[0] as Block & { t: "paragraph" }).children)).toBe('<script>alert(1)</script> <img src=x onerror="alert(1)">');

    expect(links(parseInline("[click](javascript:alert(1))"))).toEqual([]);
    expect(inlineText(parseInline("[click](javascript:alert(1))"))).toBe("click");
    expect(links(parseInline("[x](data:text/html;base64,AAAA)"))).toEqual([]);
    expect(links(parseInline("[x](vbscript:msgbox)"))).toEqual([]);
    expect(links(parseInline("[site](https://craft-it4sd.eu/replication/)"))).toEqual(["https://craft-it4sd.eu/replication/"]);
    expect(links(parseInline("<https://example.com/a>"))).toEqual(["https://example.com/a"]);
    expect(links(parseInline("see https://example.com/x."))).toEqual(["https://example.com/x"]);
    expect(links(parseInline("[me](mailto:owner@example.com)"))).toEqual(["mailto:owner@example.com"]);
    expect(links(parseInline("![pic](https://example.com/p.png)"))).toEqual(["https://example.com/p.png"]);
    expect(safeLink("JavaScript:alert(1)")).toBeNull();
    expect(safeLink("mailto:not an address")).toBeNull();
  });

  it("handles emphasis edge cases and escapes", () => {
    expect(inlineText(parseInline("snake_case_name stays"))).toBe("snake_case_name stays");
    expect(parseInline("snake_case_name").some((n) => n.t === "em")).toBe(false);
    expect(parseInline("2 * 3 * 4").some((n) => n.t === "em")).toBe(false);
    expect(inlineText(parseInline("\\*not em\\*"))).toBe("*not em*");
    expect(parseInline("`a **b**`")).toEqual([{ t: "code", text: "a **b**" }]);
    expect(parseInline("~~old~~ new")[0]).toMatchObject({ t: "del" });
    // Deep nesting does not blow the stack.
    expect(() => parseMarkdown(`${"> ".repeat(200)}deep`)).not.toThrow();
    expect(() => parseInline("[".repeat(500))).not.toThrow();
  });
});

describe("plainExcerpt", async () => {
  const { plainExcerpt, parseTags } = await import("../api/notes");
  it("drops markdown marks and a heading that repeats the title", () => {
    expect(plainExcerpt("# CRAFT — meeting *Briefing di ChatGPT* per Giuseppe", "CRAFT — meeting")).toBe("Briefing di ChatGPT per Giuseppe");
    expect(plainExcerpt("Saved by Lucina. - **Mamma**: a cookbook (*Il cucchiaio*) - [ ] Check")).toBe("Saved by Lucina. Mamma: a cookbook (Il cucchiaio) Check");
    expect(plainExcerpt("## Friday Train at 08:10 ## Saturday 1. Galleria [site](https://x.it) | Day | Budget |")).toBe("Friday Train at 08:10 Saturday Galleria site Day Budget");
    expect(plainExcerpt("snake_case_name stays")).toBe("snake_case_name stays");
    expect(plainExcerpt("*Briefing di ChatGPT, cut off before the clos…")).toBe("Briefing di ChatGPT, cut off before the clos…");
    expect(parseTags("Work, #rome , work,, ")).toEqual(["work", "rome"]);
  });
});
