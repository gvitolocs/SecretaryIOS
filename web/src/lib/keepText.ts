// Keep's "rich text basics" as markdown (keep-contract.md): **bold**, *italic*, <u>underline</u>,
// "# H1", "## H2", "- list" lines, [links](https://…) and bare URLs.
//
// Three small pieces, all free of innerHTML:
//   parseKeepText(md)       -> blocks for the card and the read-only views (components/keep/KeepText)
//   mdToDom(md, doc)        -> DOM nodes for the contenteditable body of the note dialog
//   domToMd(root)           -> markdown back from that DOM
// The formatting toolbar (bold, italic, underline, H1, H2) acts on the DOM with execCommand.

import { safeLink } from "./markdown";

export type KInline =
  | { t: "text"; text: string }
  | { t: "b" | "i" | "u"; c: KInline[] }
  | { t: "a"; href: string; c: KInline[] };

export type KBlock =
  | { t: "p"; c: KInline[] }
  | { t: "h"; level: 1 | 2; c: KInline[] }
  | { t: "li"; marker: string; indent: number; c: KInline[] }
  | { t: "task"; checked: boolean; c: KInline[] }
  | { t: "blank" };

const URL_RE = /^https?:\/\/[^\s<>\]]+/i;

function trimUrl(u: string): string {
  // Sentence punctuation and an unbalanced closing parenthesis are not part of the link.
  let s = u;
  for (;;) {
    const last = s[s.length - 1] ?? "";
    if (/[.,;:!?'"]/.test(last)) s = s.slice(0, -1);
    else if (last === ")" && (s.match(/\(/g)?.length ?? 0) < (s.match(/\)/g)?.length ?? 0)) s = s.slice(0, -1);
    else return s;
  }
}

/** Closing `*` of an italic span starting after `from`: not after a space, not part of `**`. */
function closeStar(s: string, from: number): number {
  for (let j = from; j < s.length; j++) {
    if (s[j] !== "*") continue;
    if (s[j + 1] === "*") {
      j++;
      continue;
    }
    if (j > from && !/\s/.test(s[j - 1] ?? "")) return j;
  }
  return -1;
}

export function parseInline(s: string, depth = 0): KInline[] {
  const out: KInline[] = [];
  let buf = "";
  const flush = () => {
    if (buf) out.push({ t: "text", text: buf });
    buf = "";
  };
  let i = 0;
  while (i < s.length) {
    const rest = depth < 4;
    if (rest && s.startsWith("**", i)) {
      const j = s.indexOf("**", i + 2);
      if (j > i + 2) {
        flush();
        out.push({ t: "b", c: parseInline(s.slice(i + 2, j), depth + 1) });
        i = j + 2;
        continue;
      }
    }
    if (rest && s[i] === "*" && s[i + 1] !== "*" && s[i + 1] !== " " && s[i + 1] !== undefined) {
      const j = closeStar(s, i + 1);
      if (j > i + 1) {
        flush();
        out.push({ t: "i", c: parseInline(s.slice(i + 1, j), depth + 1) });
        i = j + 1;
        continue;
      }
    }
    if (rest && s.startsWith("<u>", i)) {
      const j = s.indexOf("</u>", i + 3);
      if (j > i + 3) {
        flush();
        out.push({ t: "u", c: parseInline(s.slice(i + 3, j), depth + 1) });
        i = j + 4;
        continue;
      }
    }
    if (s[i] === "[") {
      const m = /^\[([^\]\n]+)\]\(([^)\s]+)\)/.exec(s.slice(i));
      if (m) {
        const href = m[2] ?? "";
        if (safeLink(href)) {
          flush();
          out.push({ t: "a", href, c: parseInline(m[1] ?? "", depth + 1) });
          i += m[0].length;
          continue;
        }
      }
    }
    if ((s[i] === "h" || s[i] === "H") && !/[A-Za-z0-9]/.test(s[i - 1] ?? " ")) {
      const m = URL_RE.exec(s.slice(i));
      if (m) {
        const url = trimUrl(m[0]);
        const href = url;
        if (safeLink(url) && url.length > 8) {
          flush();
          out.push({ t: "a", href, c: [{ t: "text", text: url }] });
          i += url.length;
          continue;
        }
      }
    }
    buf += s[i];
    i++;
  }
  flush();
  return out;
}

/** One block per line: Keep keeps blank lines and line breaks as typed. */
export function parseKeepText(md: string): KBlock[] {
  const blocks: KBlock[] = [];
  for (const raw of md.replace(/\r\n?/g, "\n").split("\n")) {
    const line = raw.replace(/\s+$/, "");
    if (!line.trim()) {
      blocks.push({ t: "blank" });
      continue;
    }
    let m = /^(#{1,2})\s+(.*)$/.exec(line);
    if (m) {
      blocks.push({ t: "h", level: m[1]!.length === 1 ? 1 : 2, c: parseInline(m[2] ?? "") });
      continue;
    }
    m = /^(\s*)[-*+]\s+\[([ xX])\]\s+(.*)$/.exec(line);
    if (m) {
      blocks.push({ t: "task", checked: m[2] !== " ", c: parseInline(m[3] ?? "") });
      continue;
    }
    m = /^(\s*)([-*+]|\d{1,3}[.)])\s+(.*)$/.exec(line);
    if (m) {
      const marker = /\d/.test(m[2] ?? "") ? (m[2] ?? "") : "•";
      blocks.push({ t: "li", marker, indent: Math.min(2, Math.floor((m[1]?.length ?? 0) / 2)), c: parseInline(m[3] ?? "") });
      continue;
    }
    blocks.push({ t: "p", c: parseInline(line) });
  }
  while (blocks.length && blocks[blocks.length - 1]!.t === "blank") blocks.pop();
  while (blocks.length && blocks[0]!.t === "blank") blocks.shift();
  return blocks;
}

/** Plain text of markdown (search, link detection, accessible names). */
export function plainText(md: string): string {
  return md
    .replace(/<\/?u>/g, "")
    .replace(/\[([^\]\n]+)\]\([^)\s]+\)/g, "$1")
    .replace(/^\s*#{1,2}\s+/gm, "")
    .replace(/^(\s*)[-*+]\s+\[[ xX]\]\s+/gm, "$1")
    .replace(/\*\*|(?<![\w*])\*(?=\S)|(?<=\S)\*(?![\w*])/g, "");
}

/** Every http(s) link in a text (for the card's link chips; Keep shows its annotations the same way). */
export function linksIn(md: string): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  const walk = (nodes: KInline[]) => {
    for (const n of nodes) {
      if (n.t === "a") {
        if (!seen.has(n.href)) {
          seen.add(n.href);
          out.push(n.href);
        }
      } else if (n.t !== "text") walk(n.c);
    }
  };
  for (const b of parseKeepText(md)) if (b.t !== "blank") walk(b.c);
  return out;
}

export function hostOf(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return url;
  }
}

// ---------------------------------------------------------------- DOM <-> markdown (the note dialog's body)

function inlineToDom(nodes: KInline[], doc: Document, into: Node): void {
  for (const n of nodes) {
    if (n.t === "text") into.appendChild(doc.createTextNode(n.text));
    else if (n.t === "a") {
      const a = doc.createElement("a");
      a.setAttribute("href", n.href);
      a.setAttribute("rel", "noopener noreferrer nofollow");
      a.setAttribute("target", "_blank");
      inlineToDom(n.c, doc, a);
      into.appendChild(a);
    } else {
      const el = doc.createElement(n.t);
      inlineToDom(n.c, doc, el);
      into.appendChild(el);
    }
  }
}

/** Markdown as editable DOM: a block per line (div, h1, h2). List lines stay as typed ("- item"). */
export function mdToDom(md: string, doc: Document = document): DocumentFragment {
  const frag = doc.createDocumentFragment();
  const lines = md.replace(/\r\n?/g, "\n").split("\n");
  while (lines.length > 1 && !lines[lines.length - 1]!.trim()) lines.pop();
  for (const raw of lines) {
    const h = /^(#{1,2})\s+(.*)$/.exec(raw);
    const el = doc.createElement(h ? (h[1]!.length === 1 ? "h1" : "h2") : "div");
    const text = h ? (h[2] ?? "") : raw;
    if (!text) el.appendChild(doc.createElement("br"));
    else inlineToDom(parseInline(text), doc, el);
    frag.appendChild(el);
  }
  return frag;
}

const BLOCK_TAGS = new Set(["DIV", "P", "H1", "H2", "H3", "H4", "H5", "H6", "UL", "OL", "LI", "BLOCKQUOTE", "PRE"]);

function wrapMark(mark: string, inner: string): string {
  if (!inner.trim()) return inner;
  const lead = /^\s*/.exec(inner)?.[0] ?? "";
  const trail = /\s*$/.exec(inner)?.[0] ?? "";
  const core = inner.slice(lead.length, inner.length - trail.length);
  if (mark === "u") return `${lead}<u>${core}</u>${trail}`;
  return `${lead}${mark}${core}${mark}${trail}`;
}

function inlineMd(node: Node): string {
  if (node.nodeType === 3) return (node.textContent ?? "").replace(/ /g, " ").replace(/[​\r\n]/g, "");
  if (node.nodeType !== 1) return "";
  const el = node as HTMLElement;
  const tag = el.tagName;
  if (tag === "BR") return "";
  let inner = "";
  el.childNodes.forEach((c) => {
    inner += inlineMd(c);
  });
  const style = el.getAttribute("style") ?? "";
  const bold = tag === "B" || tag === "STRONG" || /font-weight:\s*(bold|[6-9]00)/i.test(style);
  const italic = tag === "I" || tag === "EM" || /font-style:\s*italic/i.test(style);
  const under = tag === "U" || /text-decoration[^;]*underline/i.test(style);
  if (tag === "A") {
    const href = el.getAttribute("href") ?? "";
    if (!safeLink(href)) return inner;
    return inner.trim() === href || !inner.trim() ? href : `[${inner}](${href})`;
  }
  if (bold) inner = wrapMark("**", inner);
  if (italic) inner = wrapMark("*", inner);
  if (under) inner = wrapMark("u", inner);
  return inner;
}

/** The markdown for an edited body. Every block element is one line; <br> ends a line. */
export function domToMd(root: Node): string {
  const lines: string[] = [];
  let cur = "";
  let hasContent = false;
  const flush = (force = false) => {
    if (cur !== "" || hasContent || force) lines.push(cur);
    cur = "";
    hasContent = false;
  };
  const walk = (node: Node, prefix: string) => {
    node.childNodes.forEach((child) => {
      if (child.nodeType === 1) {
        const el = child as HTMLElement;
        if (el.tagName === "BR") {
          flush(true);
          return;
        }
        if (BLOCK_TAGS.has(el.tagName)) {
          flush();
          const hasBlockChild = [...el.childNodes].some((c) => c.nodeType === 1 && BLOCK_TAGS.has((c as HTMLElement).tagName));
          const pre = el.tagName === "H1" ? "# " : el.tagName === "H2" ? "## " : el.tagName === "LI" ? "- " : "";
          const softBreaks = !!el.querySelector("br") && (el.textContent ?? "").trim() !== "";
          if (hasBlockChild || softBreaks || el.tagName === "UL" || el.tagName === "OL") {
            walk(el, pre);
            flush();
            return;
          }
          const text = inlineMd(el);
          // An empty block (a <div><br></div>) is a blank line.
          lines.push(text.trim() === "" ? "" : pre + text);
          return;
        }
        cur += (cur === "" ? prefix : "") + inlineMd(el);
        hasContent = hasContent || cur !== "";
        return;
      }
      if (child.nodeType === 3) {
        const s = inlineMd(child);
        if (s) cur += (cur === "" ? prefix : "") + s;
      }
    });
  };
  walk(root, "");
  flush();
  return lines.join("\n").replace(/\s+$/, "");
}

/** Line-level helpers for the editor's formatting toolbar, shared with tests. */
export type FormatCommand = "bold" | "italic" | "underline" | "h1" | "h2" | "clear";
