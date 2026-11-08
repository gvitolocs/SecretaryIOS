/**
 * Decision questions reach her chat worded by the server (crates/lucina-server/src/decide.rs, ask_owner):
 * `claude-code (lucinamaps-courier (local_8721b9…)) needs a decision: …`, ending with how to answer by text.
 * The asker's session ids mean nothing to the owner, so the chat shows a short heading
 * ("Decision for you · claude-code · lucinamaps-courier") over the question instead. Display only: the stored
 * text, and what Telegram and the iPhone show, are unchanged.
 */

const HEAD = /^(.+?) needs a decision: /;
const HOW_TO_ANSWER = /\n*Tap an option \(or open Secretary → Decisions\), or answer "Decision ([^:"]+): <your words>"\.\s*$/;

/** "claude-code (lucinamaps-courier (local_8721b9…))" → "claude-code · lucinamaps-courier". */
export function askerLabel(raw: string): string {
  const parts = raw
    .split(/[()]/)
    .map((s) => s.trim())
    .filter((s) => s && !/^(local_|session[_-]|[0-9a-f]{8,}\b)/i.test(s));
  return [...new Set(parts)].join(" · ") || raw.trim();
}

/** One line for a notification: "claude-code · lucinamaps-courier: Lucina Maps: which engine?". */
export function decisionSummary(text: string): string {
  const m = HEAD.exec(text);
  return m ? `${askerLabel(m[1]!)}: ${text.slice(m[0].length)}` : text;
}

/** The chat text of a decision question, or `text` unchanged for any other message. */
export function tidyDecisionText(text: string): string {
  const m = HEAD.exec(text);
  if (!m) return text;
  const rest = text
    .slice(m[0].length)
    .replace(HOW_TO_ANSWER, (_all, id: string) => `\n\n_Tap an option, open Decisions, or reply “Decision ${id}: …”._`);
  return `**Decision for you** · ${askerLabel(m[1]!)}\n\n${rest}`;
}
