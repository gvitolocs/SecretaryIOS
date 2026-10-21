// Split plain text into text and link segments. Links are rendered as real
// <a> elements with text children, never through innerHTML, and only http(s)
// URLs are ever produced.

export type Segment =
  | { kind: "text"; text: string }
  | { kind: "link"; text: string; href: string };

const URL_RE = /(?:https?:\/\/|www\.)[^\s<>"'`]+/gi;
const TRAILING_PUNCT = /[.,;:!?'"*_~]+$/;
const PAIRS: Record<string, string> = { ")": "(", "]": "[", "}": "{" };

function count(s: string, ch: string): number {
  let n = 0;
  for (const c of s) if (c === ch) n++;
  return n;
}

function trimUrl(raw: string): string {
  let url = raw;
  for (;;) {
    const before = url;
    url = url.replace(TRAILING_PUNCT, "");
    const last = url.at(-1);
    if (last && last in PAIRS) {
      const open = PAIRS[last] ?? "";
      if (count(url, last) > count(url, open)) url = url.slice(0, -1);
    }
    if (url === before) return url;
  }
}

export function safeHref(text: string): string | null {
  const candidate = /^www\./i.test(text) ? `https://${text}` : text;
  try {
    const parsed = new URL(candidate);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return null;
    const host = parsed.hostname;
    if (!host || (!host.includes(".") && host !== "localhost")) return null;
    return parsed.href;
  } catch {
    return null;
  }
}

export function linkify(input: string): Segment[] {
  const out: Segment[] = [];
  let cursor = 0;
  for (const match of input.matchAll(URL_RE)) {
    const start = match.index ?? 0;
    // Do not link the tail of a word such as "foowww.example.com".
    if (start > 0 && /[\w@/]/.test(input[start - 1] ?? "")) continue;
    const text = trimUrl(match[0]);
    const href = safeHref(text);
    if (!href) continue;
    if (start > cursor) out.push({ kind: "text", text: input.slice(cursor, start) });
    out.push({ kind: "link", text, href });
    cursor = start + text.length;
  }
  if (cursor < input.length) out.push({ kind: "text", text: input.slice(cursor) });
  return out;
}
