import { describe, expect, it } from "vitest";
import { citationRefs, inlineText, parseInline, parseMarkdown, safeLink, type Block, type Inline } from "./markdown";

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
    expect(plainExcerpt("a cookbook (*Il cucchiaio d'argento*) Marco")).toBe("a cookbook (Il cucchiaio d'argento) Marco");
    expect(plainExcerpt("*Briefing di ChatGPT, cut off before the clos…")).toBe("Briefing di ChatGPT, cut off before the clos…");
    expect(parseTags("Work, #rome , work,, ")).toEqual(["work", "rome"]);
  });
});

describe("[note:<slug>] citations", () => {
  const cite = (src: string) => parseInline(src).filter((n) => n.t === "link");

  it("turns a citation into an in-app link to the note in Keep", () => {
    const nodes = parseInline("Saved it in [note:gift-ideas-xmas] for you.");
    expect(nodes).toEqual([
      { t: "text", text: "Saved it in " },
      { t: "link", href: "/keep/note/gift-ideas-xmas", route: true, children: [{ t: "text", text: "note:gift-ideas-xmas" }] },
      { t: "text", text: " for you." },
    ]);
  });

  it("finds several, and keeps the rest of the text, bold and links as they are", () => {
    const nodes = parseInline("**See** [note:a] and [note:b-2], also [the site](https://x.org).");
    expect(links(nodes)).toEqual(["/keep/note/a", "/keep/note/b-2", "https://x.org/"]);
    expect(nodes[0]).toMatchObject({ t: "strong" });
  });

  it("only accepts a slug of [a-z0-9-], 1 to 64 characters", () => {
    expect(cite("[note:Upper]")).toHaveLength(0);
    expect(cite("[note:with space]")).toHaveLength(0);
    expect(cite("[note:]")).toHaveLength(0);
    expect(cite("[note:a_b]")).toHaveLength(0);
    expect(cite(`[note:${"a".repeat(64)}]`)).toHaveLength(1);
    expect(cite(`[note:${"a".repeat(65)}]`)).toHaveLength(0);
    expect(inlineText(parseInline("[note:Upper]"))).toBe("[note:Upper]");
  });

  it("leaves code and a markdown link that happens to start with note: alone", () => {
    expect(links(parseInline("`[note:x]`"))).toEqual([]);
    expect(links(parseInline("[note:x](https://y.org)"))).toEqual(["https://y.org/"]);
  });

  it("is inline in paragraphs and list items of a whole document", () => {
    const blocks = parseMarkdown("- one [note:x]\n- two");
    const first = (blocks[0] as Extract<Block, { t: "list" }>).items[0]!.children[0] as Extract<Block, { t: "paragraph" }>;
    expect(links(first.children)).toEqual(["/keep/note/x"]);
  });
});


describe("[source:<chunk id>] and [insight:<id>] citations (Notebooks)", () => {
  it("turns them into cite nodes that keep the ref as written", () => {
    expect(parseInline("True [source:s0a9be3c112-14].")).toEqual([
      { t: "text", text: "True " },
      { t: "cite", kind: "source", id: "s0a9be3c112-14", ref: "source:s0a9be3c112-14" },
      { t: "text", text: "." },
    ]);
    expect(parseInline("[insight:i4f1c09a2e7]")).toEqual([{ t: "cite", kind: "insight", id: "i4f1c09a2e7", ref: "insight:i4f1c09a2e7" }]);
  });

  it("keeps [note:<slug>] as the link to Keep, next to the new ones", () => {
    const nodes = parseInline("a [source:s1-2] b [note:my-note] c [source:s1-9]");
    expect(nodes.map((n) => n.t)).toEqual(["text", "cite", "text", "link", "text", "cite"]);
    expect(links(nodes)).toEqual(["/keep/note/my-note"]);
  });

  it("leaves lookalikes, markdown links and code as they are", () => {
    expect(parseInline("[source:]").some((n) => n.t === "cite")).toBe(false);
    expect(parseInline("[source:a b]").some((n) => n.t === "cite")).toBe(false);
    expect(parseInline("[other:s1-2]").some((n) => n.t === "cite")).toBe(false);
    expect(parseInline("`[source:s1-2]`")).toEqual([{ t: "code", text: "[source:s1-2]" }]);
    expect(links(parseInline("[source:s1-2](https://y.org)"))).toEqual(["https://y.org/"]);
    expect(inlineText(parseInline("[source:]"))).toBe("[source:]");
  });

  it("reads as [ref] in plain text", () => {
    expect(inlineText(parseInline("see [source:s1-2]"))).toBe("see [source:s1-2]");
  });

  it("lists each cited ref once, in the order the chips are numbered, through lists, quotes and tables", () => {
    const doc = ["First [source:s1-4] and [insight:i1].", "", "- item [source:s1-2]", "- again [source:s1-4]", "", "> quoted [source:s2-0]", "", "| a |", "|---|", "| [source:s3-1] |"].join("\n");
    expect(citationRefs(parseMarkdown(doc))).toEqual(["source:s1-4", "insight:i1", "source:s1-2", "source:s2-0", "source:s3-1"]);
    expect(citationRefs(parseMarkdown("nothing here"))).toEqual([]);
  });
});

describe("calendar links", () => {
  const route = (src: string) => parseInline(src).find((n) => n.t === "link");
  const eid = btoa("abc123 owner@example.com").replace(/=+$/, "");

  it("opens Lucina's /calendar links in the app", () => {
    expect(route("[Open in calendar](https://getsecretary.tech/calendar?event=lucina%3Aev_abc)")).toEqual({
      t: "link",
      href: "/calendar?event=lucina%3Aev_abc",
      route: true,
      children: [{ t: "text", text: "Open in calendar" }],
    });
    expect(route(`[Open](${window.location.origin}/calendar?event=lucina%3Aev_abc)`)).toMatchObject({
      href: "/calendar?event=lucina%3Aev_abc",
      route: true,
    });
  });

  it("opens Google Calendar event links on the event in the app", () => {
    expect(route(`[Open in Google Calendar](https://calendar.google.com/calendar/event?eid=${eid})`)).toMatchObject({
      href: "/calendar?event=google%3Aabc123",
      route: true,
    });
  });

  it("leaves other links alone", () => {
    const other = route("[Shop](https://getsecretary.tech/shopping)");
    expect(other).toEqual({ t: "link", href: "https://getsecretary.tech/shopping", children: [{ t: "text", text: "Shop" }] });
    expect(route("<https://example.com/calendar>")).not.toHaveProperty("route");
  });
});
