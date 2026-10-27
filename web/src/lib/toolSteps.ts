// Lucina's tool calls in the chat ("🔧 drive {…} → ok", meta.kind "tool") are steps of her turn, not messages.
// A run of them between two messages shows as one quiet line, "Worked on Drive · 4 steps", that opens to the steps.

import type { ChatMessage } from "../state/chat";

/** A run of tool steps shown as one line; `first` is its first step's key. */
export interface ToolRun {
  kind: "tools";
  key: string;
  first: string;
  steps: ChatMessage[];
}

/** A chat row: a message as stored (so <For> keeps its component), or a run of tool steps. */
export type ChatRow = ChatMessage | ToolRun;

export function isToolRun(row: ChatRow): row is ToolRun {
  return (row as ToolRun).kind === "tools";
}

/** A tool step: Lucina's item with meta.kind "tool". */
export function isToolStep(m: ChatMessage): boolean {
  return m.role === "lucina" && m.meta?.kind === "tool";
}

/**
 * Messages with every run of consecutive tool steps folded into one row. Pass the same `cache` on every call
 * (one per list): an unchanged run is then the same object, so the list does not rebuild its line.
 */
export function chatRows(messages: readonly ChatMessage[], cache?: Map<string, ToolRun>): ChatRow[] {
  const rows: ChatRow[] = [];
  let run: ChatMessage[] = [];
  const used = new Set<string>();
  const flush = () => {
    if (!run.length) return;
    const first = run[0]!.key;
    const id = `${first}:${run.length}`;
    let row = cache?.get(id);
    if (!row) {
      row = { kind: "tools", key: `tools:${first}`, first, steps: run };
      cache?.set(id, row);
    }
    used.add(id);
    rows.push(row);
    run = [];
  };
  for (const m of messages) {
    if (isToolStep(m)) {
      run.push(m);
      continue;
    }
    flush();
    rows.push(m);
  }
  flush();
  if (cache) for (const id of cache.keys()) if (!used.has(id)) cache.delete(id);
  return rows;
}

const NAMES: Record<string, string> = {
  drive: "Drive",
  media: "Gallery",
  gallery: "Gallery",
  memo: "Memos",
  memos: "Memos",
  meshy: "3D models",
  calendar: "Calendar",
  computer: "Computer",
  android: "Android",
  shopping: "Shopping",
  reminders: "Reminders",
  notes: "Notes",
  tasks: "Tasks",
  maps: "Maps",
  secrets: "Secrets",
};

/** "Drive" for `drive`, "Web search" for `web_search`. */
export function toolName(tool: string): string {
  const known = NAMES[tool.toLowerCase()];
  if (known) return known;
  const words = tool.replace(/[_-]+/g, " ").trim();
  return words ? words[0]!.toUpperCase() + words.slice(1) : tool;
}

/** The tools a run used, in first-use order, and how many steps failed. */
export function toolSummary(steps: readonly ChatMessage[]): { tools: string[]; failed: number } {
  const tools: string[] = [];
  let failed = 0;
  for (const s of steps) {
    const name = toolName(s.meta?.tool ?? stepTool(s.text) ?? "tool");
    if (!tools.includes(name)) tools.push(name);
    if (s.meta?.ok === false) failed++;
  }
  return { tools, failed };
}

/** A step's text without the wrench: `drive {"action":"list"} → ok`. */
export function stepText(m: ChatMessage): string {
  return m.text.replace(/^\s*🔧\s*/u, "");
}

function stepTool(text: string): string | undefined {
  return /^\s*(?:🔧\s*)?([A-Za-z][\w-]*)/u.exec(text)?.[1];
}
