// A small, safe Markdown parser for notes. It builds a tree of plain data that
// components/Markdown.tsx renders as real elements with text children: nothing
// is ever parsed or inserted as HTML (raw HTML shows as text), and links are
// only produced for http(s) and mailto URLs.
//
// Supported: ATX headings, paragraphs (single line breaks kept), bullet and
// numbered lists (nested by indentation, `[ ]`/`[x]` task items), block
// quotes, fenced code, horizontal rules, simple pipe tables; inline code,
// **bold**, *italic*, ~~strike~~, [links](url), <autolinks> and bare URLs.
// Images become links to the image.

import { linkify, safeHref } from "./linkify";

export type Inline =
  | { t: "text"; text: string }
  | { t: "br" }
  | { t: "code"; text: string }
  | { t: "strong"; children: Inline[] }
  | { t: "em"; children: Inline[] }
  | { t: "del"; children: Inline[] }
  | { t: "link"; href: string; children: Inline[]; /** An in-app route (a citation like [note:slug]): opened in the app, not in a new tab. */ route?: true };

export interface ListItem {
  children: Block[];
  /** Task item: `[ ]` false, `[x]` true. */
  checked?: boolean;
}

export type Block =
  | { t: "heading"; level: 1 | 2 | 3 | 4 | 5 | 6; children: Inline[] }
  | { t: "paragraph"; children: Inline[] }
  | { t: "list"; ordered: boolean; start: number; items: ListItem[] }
  | { t: "quote"; children: Block[] }
  | { t: "code"; lang: string; text: string }
  | { t: "hr" }
  | { t: "table"; head: Inline[][]; rows: Inline[][][]; align: ("left" | "center" | "right" | null)[] };

const MAX_DEPTH = 8;

/** `[note:<slug>]`, a slug being `[a-z0-9-]{1,64}`. */
const NOTE_CITE = /^\[note:([a-z0-9-]{1,64})\]/;

/** `mailto:` with an address, or what `safeHref` accepts. */
export function safeLink(raw: string): string | null {
  const url = raw.trim().replace(/^<|>$/g, "");
  if (/^mailto:/i.test(url)) {
    return /^mailto:[^\s@<>()"']+@[^\s@<>()"']+$/i.test(url) ? url : null;
  }
  if (/^[a-z][a-z0-9+.-]*:/i.test(url) && !/^https?:/i.test(url)) return null;
  return safeHref(url);
}

// ---------------------------------------------------------------- inline

function textNodes(text: string): Inline[] {
  // Bare URLs become links (linkify only yields http(s)).
  const out: Inline[] = [];
  for (const seg of linkify(text)) {
    if (seg.kind === "link") out.push({ t: "link", href: seg.href, children: [{ t: "text", text: seg.text }] });
    else pushText(out, seg.text);
  }
  return out;
}

function pushText(out: Inline[], text: string): void {
  if (!text) return;
  const parts = text.split("\n");
  parts.forEach((p, i) => {
    if (i > 0) out.push({ t: "br" });
    if (!p) return;
    const last = out[out.length - 1];
    if (last && last.t === "text") last.text += p;
    else out.push({ t: "text", text: p });
  });
}

/** Index of the closing `]` matching the `[` at `open`, or -1. */
function closeBracket(s: string, open: number): number {
  let depth = 0;
  for (let i = open; i < s.length; i++) {
    const c = s[i];
    if (c === "\\") {
      i++;
      continue;
    }
    if (c === "[") depth++;
    else if (c === "]" && --depth === 0) return i;
  }
  return -1;
}

/** `(url "title")` after a link text: [url, end index] or null. */
function linkTarget(s: string, at: number): [string, number] | null {
  if (s[at] !== "(") return null;
  let depth = 0;
  for (let i = at; i < s.length; i++) {
    const c = s[i];
    if (c === "(") depth++;
    else if (c === ")" && --depth === 0) {
      const inner = s.slice(at + 1, i).trim();
      const url = inner.split(/\s+/)[0] ?? "";
      return [url, i + 1];
    } else if (c === "\n") return null;
  }
  return null;
}

export function parseInline(src: string, depth = 0): Inline[] {
  const out: Inline[] = [];
  if (depth > MAX_DEPTH) {
    pushText(out, src);
    return out;
  }
  let buf = "";
  const flush = () => {
    if (buf) out.push(...textNodes(buf));
    buf = "";
  };
  let i = 0;
  while (i < src.length) {
    const c = src[i]!;
    const rest = src.slice(i);
    if (c === "\\" && i + 1 < src.length && /[\\`*_{}[\]()#+\-.!~|>]/.test(src[i + 1]!)) {
      buf += src[i + 1];
      i += 2;
      continue;
    }
    if (c === "`") {
      const m = /^(`+)([\s\S]*?[^`])\1(?!`)/.exec(rest);
      if (m) {
        flush();
        out.push({ t: "code", text: m[2]!.replace(/\n/g, " ").trim() || m[2]! });
        i += m[0].length;
        continue;
      }
    }
    if (c === "<") {
      const m = /^<((?:https?:\/\/|mailto:)[^\s<>]+)>/i.exec(rest);
      if (m) {
        const href = safeLink(m[1]!);
        if (href) {
          flush();
          out.push({ t: "link", href, children: [{ t: "text", text: m[1]!.replace(/^mailto:/i, "") }] });
          i += m[0].length;
          continue;
        }
      }
    }
    if (c === "!" && src[i + 1] === "[") {
      const close = closeBracket(src, i + 1);
      const target = close > 0 ? linkTarget(src, close + 1) : null;
      if (target) {
        const alt = src.slice(i + 2, close) || target[0];
        const href = safeLink(target[0]);
        flush();
        if (href) out.push({ t: "link", href, children: [{ t: "text", text: `🖼 ${alt}` }] });
        else pushText(out, alt);
        i = target[1];
        continue;
      }
    }
    if (c === "[") {
      // Lucina cites a note as [note:<slug>]: a link that opens it in Keep.
      const cite = NOTE_CITE.exec(rest);
      if (cite && src[i + cite[0].length] !== "(") {
        flush();
        out.push({ t: "link", href: `/keep/note/${cite[1]}`, route: true, children: [{ t: "text", text: `note:${cite[1]}` }] });
        i += cite[0].length;
        continue;
      }
      const close = closeBracket(src, i);
      const target = close > 0 ? linkTarget(src, close + 1) : null;
      if (target) {
        const children = parseInline(src.slice(i + 1, close), depth + 1);
        const href = safeLink(target[0]);
        flush();
        // An unsafe target keeps its text, without a link.
        if (href) out.push({ t: "link", href, children: children.length ? children : [{ t: "text", text: target[0] }] });
        else out.push(...children);
        i = target[1];
        continue;
      }
    }
    if (c === "~" && src[i + 1] === "~") {
      const end = src.indexOf("~~", i + 2);
      if (end > i + 2) {
        flush();
        out.push({ t: "del", children: parseInline(src.slice(i + 2, end), depth + 1) });
        i = end + 2;
        continue;
      }
    }
    if (c === "*" || c === "_") {
      const double = src[i + 1] === c;
      const mark = double ? c + c : c;
      // `_` inside words (snake_case) is not emphasis.
      const prev = src[i - 1] ?? " ";
      const opensOk = c === "*" || !/[\p{L}\p{N}]/u.test(prev);
      const next = src[i + mark.length] ?? " ";
      if (opensOk && next !== " " && next !== "\n") {
        let end = src.indexOf(mark, i + mark.length);
        // `*a **b** c*`: skip a double marker when looking for a single one.
        while (!double && end !== -1 && src[end + 1] === c) end = src.indexOf(mark, end + 2);
        const after = end === -1 ? "" : (src[end + mark.length] ?? " ");
        const closesOk = end > i + mark.length && src[end - 1] !== " " && (c === "*" || !/[\p{L}\p{N}]/u.test(after));
        if (closesOk) {
          flush();
          const children = parseInline(src.slice(i + mark.length, end), depth + 1);
          out.push(double ? { t: "strong", children } : { t: "em", children });
          i = end + mark.length;
          continue;
        }
      }
    }
    buf += c;
    i++;
  }
  flush();
  return out;
}

// ---------------------------------------------------------------- blocks

const FENCE = /^ {0,3}(`{3,}|~{3,})\s*([\w+-]*)/;
const HEADING = /^ {0,3}(#{1,6})(?:\s+(.*?))?\s*#*\s*$/;
const HR = /^ {0,3}([-*_])(?:\s*\1){2,}\s*$/;
const QUOTE = /^ {0,3}>\s?/;
const BULLET = /^( *)([-*+])\s+(.*)$/;
const ORDERED = /^( *)(\d{1,9})[.)]\s+(.*)$/;
const TABLE_SEP = /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/;

function isBlank(line: string): boolean {
  return line.trim() === "";
}

function listMarker(line: string): { indent: number; ordered: boolean; start: number; text: string } | null {
  const b = BULLET.exec(line);
  if (b && !HR.test(line)) return { indent: b[1]!.length, ordered: false, start: 1, text: b[3]! };
  const o = ORDERED.exec(line);
  if (o) return { indent: o[1]!.length, ordered: true, start: Number(o[2]), text: o[3]! };
  return null;
}

function splitRow(line: string): string[] {
  let s = line.trim();
  if (s.startsWith("|")) s = s.slice(1);
  if (s.endsWith("|") && !s.endsWith("\\|")) s = s.slice(0, -1);
  const cells: string[] = [];
  let cur = "";
  for (let i = 0; i < s.length; i++) {
    if (s[i] === "\\" && s[i + 1] === "|") {
      cur += "|";
      i++;
    } else if (s[i] === "|") {
      cells.push(cur.trim());
      cur = "";
    } else cur += s[i];
  }
  cells.push(cur.trim());
  return cells;
}

function startsBlock(line: string): boolean {
  return FENCE.test(line) || HEADING.test(line) || HR.test(line) || QUOTE.test(line) || listMarker(line) !== null;
}

function parseLines(lines: string[], depth: number): Block[] {
  const blocks: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i]!;
    if (isBlank(line)) {
      i++;
      continue;
    }
    const fence = FENCE.exec(line);
    if (fence) {
      const marker = fence[1]!;
      const body: string[] = [];
      i++;
      while (i < lines.length && !lines[i]!.trimStart().startsWith(marker)) body.push(lines[i++]!);
      i++; // closing fence (or the end)
      blocks.push({ t: "code", lang: fence[2] ?? "", text: body.join("\n") });
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading) {
      const level = heading[1]!.length as 1 | 2 | 3 | 4 | 5 | 6;
      blocks.push({ t: "heading", level, children: parseInline(heading[2] ?? "", 0) });
      i++;
      continue;
    }
    if (HR.test(line)) {
      blocks.push({ t: "hr" });
      i++;
      continue;
    }
    if (QUOTE.test(line)) {
      const body: string[] = [];
      while (i < lines.length && !isBlank(lines[i]!) && (QUOTE.test(lines[i]!) || !startsBlock(lines[i]!))) {
        body.push(lines[i]!.replace(QUOTE, ""));
        i++;
      }
      blocks.push({ t: "quote", children: depth < MAX_DEPTH ? parseLines(body, depth + 1) : [{ t: "paragraph", children: [{ t: "text", text: body.join("\n") }] }] });
      continue;
    }
    const first = listMarker(line);
    if (first) {
      const list: Block & { t: "list" } = { t: "list", ordered: first.ordered, start: first.start, items: [] };
      const base = first.indent;
      while (i < lines.length) {
        const m = listMarker(lines[i]!);
        if (!m || m.indent !== base || m.ordered !== list.ordered) break;
        const body = [m.text];
        i++;
        // Continuation and nested lines: indented deeper, or a lazy paragraph line.
        while (i < lines.length) {
          const l = lines[i]!;
          if (isBlank(l)) {
            const next = lines[i + 1];
            if (next !== undefined && !isBlank(next) && next.length - next.trimStart().length > base) {
              body.push("");
              i++;
              continue;
            }
            break;
          }
          const indent = l.length - l.trimStart().length;
          const sub = listMarker(l);
          if (sub && sub.indent <= base) break;
          if (!sub && indent <= base && startsBlock(l)) break;
          body.push(indent > base ? l.slice(Math.min(indent, base + 2)) : l.trimStart());
          i++;
        }
        const item: ListItem = { children: [] };
        const task = /^\[([ xX])\]\s+/.exec(body[0]!);
        if (task) {
          item.checked = task[1] !== " ";
          body[0] = body[0]!.slice(task[0].length);
        }
        item.children = depth < MAX_DEPTH ? parseLines(body, depth + 1) : [{ t: "paragraph", children: [{ t: "text", text: body.join("\n") }] }];
        list.items.push(item);
      }
      blocks.push(list);
      continue;
    }
    // Table: a header row, then a separator row.
    if (line.includes("|") && i + 1 < lines.length && TABLE_SEP.test(lines[i + 1]!)) {
      const head = splitRow(line);
      const align = splitRow(lines[i + 1]!).map((c) => {
        const l = c.startsWith(":");
        const r = c.endsWith(":");
        return l && r ? "center" : r ? "right" : l ? "left" : null;
      });
      i += 2;
      const rows: Inline[][][] = [];
      while (i < lines.length && !isBlank(lines[i]!) && lines[i]!.includes("|")) {
        const cells = splitRow(lines[i]!);
        rows.push(head.map((_, k) => parseInline(cells[k] ?? "", 0)));
        i++;
      }
      blocks.push({ t: "table", head: head.map((h) => parseInline(h, 0)), rows, align: head.map((_, k) => align[k] ?? null) });
      continue;
    }
    // Paragraph: until a blank line or another block.
    const body: string[] = [line.trim()];
    i++;
    while (i < lines.length && !isBlank(lines[i]!) && !startsBlock(lines[i]!)) {
      if (lines[i]!.includes("|") && TABLE_SEP.test(lines[i + 1] ?? "")) break;
      body.push(lines[i]!.trim());
      i++;
    }
    blocks.push({ t: "paragraph", children: parseInline(body.join("\n"), 0) });
  }
  return blocks;
}

export function parseMarkdown(src: string): Block[] {
  const text = String(src ?? "").replace(/\r\n?/g, "\n").replace(/\t/g, "    ");
  return parseLines(text.split("\n"), 0);
}

/** Plain text of inline nodes (for titles and tests). */
export function inlineText(nodes: Inline[]): string {
  return nodes
    .map((n) => {
      if (n.t === "text" || n.t === "code") return n.text;
      if (n.t === "br") return "\n";
      return inlineText(n.children);
    })
    .join("");
}
