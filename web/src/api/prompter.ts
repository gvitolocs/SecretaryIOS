// Lucina Prompter (docs/prompter.md): scripts are Keep notes with the label "prompter"
// (secretary-server on p1, the same note store Lucina reads); drafting and summaries are
// lucina-memo's /v1/memos/prompter/* (Lucina's own model), through p1's @memos gate.

import { request } from "./client";
import { notesApi, type Note, type NoteSummary } from "./notes";
import { PROMPTER_LABEL } from "../lib/prompterScript";

const CLIENT = { "X-Secretary-Client": "web" };

export const prompterApi = {
  scripts: (): Promise<NoteSummary[]> => notesApi.list("", PROMPTER_LABEL),
  get: (key: string): Promise<Note> => notesApi.get(key),
  create: (title: string, content: string): Promise<Note> => notesApi.create({ title, content, tags: [PROMPTER_LABEL] }),
  save: (n: Note, patch: { title?: string; content?: string }): Promise<Note> => notesApi.update(n.id, patch, n.updatedAt),
  /** An existing note becomes a script: it gets the label, its text stays. */
  adopt: (n: Note): Promise<Note> =>
    n.tags.includes(PROMPTER_LABEL) ? Promise.resolve(n) : notesApi.update(n.id, { tags: [...n.tags, PROMPTER_LABEL] }, n.updatedAt),
  draft: async (topic: string, document = "", language = ""): Promise<string> =>
    (
      await request<{ markdown: string }>("POST", "/v1/memos/prompter/draft", {
        body: { topic, document, language },
        headers: CLIENT,
        timeoutMs: 90_000,
      })
    ).markdown,
  summary: async (b: { title: string; script: string; report: string; transcript: string }): Promise<string> =>
    (
      await request<{ summary: string }>("POST", "/v1/memos/prompter/summary", {
        body: b,
        headers: CLIENT,
        timeoutMs: 90_000,
      })
    ).summary,
};
