// Quick specs typed into the Shopping search field: "pane x2", "2x latte", "farina 1 kg",
// "uova 6", "pane, integrale". The item part is searched; the spec goes on the row.

export interface ParsedQuery {
  /** What to search for / the item name. */
  text: string;
  /** The spec read from the query ("x2", "1 kg", "integrale"); "" when there is none. */
  spec: string;
}

const UNITS = "kg|g|gr|hg|l|lt|ml|cl|dl|pz|pcs|stk|pk|pack";
const AMOUNT = new RegExp(`^(\\d+(?:[.,]\\d+)?)\\s?(${UNITS})\\.?$`, "i");
const TIMES = /^(?:x|×)\s?(\d{1,3})$|^(\d{1,3})\s?(?:x|×)$/i;
const COUNT = /^\d{1,2}$/;

function specOfToken(token: string): string | null {
  const times = TIMES.exec(token);
  if (times) return `x${times[1] ?? times[2]}`;
  const amount = AMOUNT.exec(token);
  if (amount) return `${amount[1]!.replace(".", ",")} ${amount[2]!.toLowerCase().replace(/^gr$/, "g").replace(/^lt$/, "l")}`;
  return null;
}

/** Splits a typed query into the item text and a quick spec. Never returns an empty text for a non-empty query. */
export function parseQuery(query: string): ParsedQuery {
  const raw = query.trim().replace(/\s+/g, " ");
  if (!raw) return { text: "", spec: "" };

  // "pane, integrale": everything after the first comma is the spec.
  const comma = raw.indexOf(",");
  if (comma > 0) {
    const text = raw.slice(0, comma).trim();
    const rest = raw.slice(comma + 1).trim();
    if (text) {
      const inner = parseQuery(text);
      return { text: inner.text, spec: [inner.spec, rest].filter(Boolean).join(" ") };
    }
  }

  // An amount at the end ("farina 1 kg", "latte 2l", "pane x2", "uova 6") or at the start ("2x latte").
  const words = raw.split(" ");
  const tryTail = (n: number): ParsedQuery | null => {
    if (words.length <= n) return null;
    const token = words.slice(-n).join(" ");
    const spec = specOfToken(token) ?? (n === 1 && COUNT.test(token) && Number(token) > 0 ? `x${Number(token)}` : null);
    return spec ? { text: words.slice(0, -n).join(" "), spec } : null;
  };
  const tryHead = (n: number): ParsedQuery | null => {
    if (words.length <= n) return null;
    const spec = specOfToken(words.slice(0, n).join(" "));
    return spec ? { text: words.slice(n).join(" "), spec } : null;
  };
  return tryTail(2) ?? tryTail(1) ?? tryHead(2) ?? tryHead(1) ?? { text: raw, spec: "" };
}

/** Joins the typed spec with the quick-spec chips that are on, without repeating one. */
export function joinSpec(typed: string, chips: readonly string[]): string {
  const parts = [typed, ...chips].map((p) => p.trim()).filter(Boolean);
  return [...new Set(parts)].join(" ");
}
