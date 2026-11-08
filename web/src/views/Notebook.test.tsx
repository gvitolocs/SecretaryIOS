import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { NotebookDetail } from "../api/notebooks";
import {
  chat,
  chatDetail,
  citation,
  detail,
  insight,
  message,
  noteLink,
  podcast,
  source,
  sse,
  stubNotebooks,
  transformation,
  type NbCall,
  type NbReply,
} from "../test/notebooksFixtures";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import NotebookView from "./Notebook";

const routes = () => (
  <>
    <Route path="/notebooks" component={() => <p>The list</p>} />
    <Route path="/notebooks/:id" component={NotebookView} />
  </>
);

/** A notebook API with some state; `extra` answers first and may return undefined to fall through. */
function serve(state: { nb?: NotebookDetail; extra?: (c: NbCall) => NbReply | undefined } = {}) {
  const s = { nb: state.nb ?? detail(), messages: chatDetail().messages };
  const calls = stubNotebooks((c) => {
    const x = state.extra?.(c);
    if (x) return x;
    const p = c.path;
    if (p === "/v1/notebooks/nb1" && c.method === "GET") return { body: s.nb };
    if (p === "/v1/notebooks/chats/c1" && c.method === "GET") return { body: chatDetail({ messages: s.messages }) };
    if (p === "/v1/notebooks/transformations") return { body: { transformations: [transformation(), transformation({ id: "t-key-insights", name: "Key Insights" })] } };
    if (p === "/v1/notebooks/sources/s1" && c.method === "GET") return { body: source() };
    if (p === "/v1/notebooks/sources/s1/insights" && c.method === "GET") return { body: { insights: [insight()] } };
    if (p === "/v1/notebooks/sources/s1/chunks") {
      return {
        body: {
          chunks: [
            { id: "s1-2", ordinal: 2, text: "Passage two text.", page: 6 },
            { id: "s1-3", ordinal: 3, text: "Reliability is about consistency.", page: 7 },
          ],
        },
      };
    }
    if (p === "/v1/notebooks/podcast-profiles") return { body: { episode_profiles: [{ id: "two-hosts", name: "Two hosts" }, { id: "deep-dive", name: "Deep dive" }], speaker_profiles: [{ id: "lucina-duo", name: "Lucina and Marco" }] } };
    if (p === "/v1/notes") return { body: { notes: [{ id: "n1", slug: "gift-ideas", title: "Gift ideas", excerpt: "Ideas for Marco" }, { id: "n2", slug: "rm-summary", title: "RM summary", excerpt: "x" }] } };
    if (c.method !== "GET") return { status: 201, body: { ok: true, slug: "saved-note", title: "Saved", excerpt: "", updated_at: "2026-10-29T10:00:00Z", indexed: true } };
    return { status: 404, body: { error: { code: "not_found", message: "Not found." } } };
  });
  return { calls, state: s };
}

const open = async (path = "/notebooks/nb1") => {
  const view = renderAt(path, routes);
  await screen.findByRole("heading", { level: 1, name: "Research Methods" });
  return view;
};
const col = (name: string) => screen.getByRole("region", { name });

afterEach(() => vi.useRealTimers());

describe("Notebook on a desktop", () => {
  it("puts Sources, Notes and Chat side by side, with the one accent action in the header", async () => {
    stubWide();
    serve();
    await open();
    expect(screen.getByText("Lecture notes · 2 sources · 1 note")).toHaveClass("pg-context");
    expect(screen.queryByRole("navigation", { name: "Research Methods" })).toBeNull();
    expect(document.querySelectorAll(".nb-cols--wide > section")).toHaveLength(3);
    expect(screen.getByRole("link", { name: /Notebooks/ })).toHaveAttribute("href", "/notebooks");
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(1);
    expect(document.querySelector(".pg-cta")).toHaveTextContent("Add source");
    expect(screen.getByRole("link", { name: "Open Notebook by Luis Novo" })).toHaveAttribute("href", "https://github.com/lfnovo/open-notebook");
    // Sources.
    const sources = within(col("Sources"));
    expect(sources.getByRole("button", { name: "RM Class 2.pdf" })).toBeInTheDocument();
    expect(sources.getByText(/PDF · RM Class 2\.pdf · 8 passages · 22 pages/)).toBeInTheDocument();
    expect(sources.getByText(/Web page · guide\.example · 4 passages/)).toBeInTheDocument();
    // Notes open in Keep.
    expect(within(col("Notes")).getByRole("link", { name: "RM summary" })).toHaveAttribute("href", "/keep/note/rm-summary");
    // Chat shows the cited answer.
    const chatCol = within(col("Chat"));
    expect(await chatCol.findByText("What is reliability?")).toBeInTheDocument();
    expect(chatCol.getByRole("button", { name: "Citation 1: RM Class 2.pdf" })).toHaveTextContent("1");
  });

  it("shows a source's state: busy, failed with its reason and a retry, and per-source context", async () => {
    stubWide();
    const { calls } = serve({
      nb: detail({
        sources: [
          source({ id: "s1", status: "ready", context: "summary" }),
          source({ id: "s2", title: "Slides", status: "embedding", chunks: 0 }),
          source({ id: "s3", title: "Scan.pdf", status: "failed", error: "No text could be extracted." }),
        ],
      }),
    });
    await open();
    const sources = within(col("Sources"));
    expect(sources.getByText("Indexing…")).toHaveAttribute("data-busy");
    expect(sources.getByText("Failed")).toHaveAttribute("data-tone", "bad");
    expect(sources.getByRole("alert")).toHaveTextContent("No text could be extracted.");
    // The context selector shows the stored value and PATCHes a change.
    const ctx = sources.getByLabelText("Use RM Class 2.pdf in the chat") as HTMLSelectElement;
    expect(ctx.value).toBe("summary");
    fireEvent.change(ctx, { target: { value: "off" } });
    await waitFor(() => expect(calls.some((c) => c.method === "PATCH" && c.url === "/v1/notebooks/nb1/sources/s1")).toBe(true));
    expect(calls.find((c) => c.method === "PATCH")!.body).toEqual({ context: "off" });
    // Retry.
    fireEvent.click(sources.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/notebooks/sources/s3/retry")).toBe(true));
  });

  it("removes a source only after asking", async () => {
    stubWide();
    const { calls } = serve();
    vi.stubGlobal("confirm", vi.fn(() => false));
    await open();
    const button = within(col("Sources")).getByRole("button", { name: "Remove RM Class 2.pdf" });
    fireEvent.click(button);
    expect(calls.some((c) => c.method === "DELETE")).toBe(false);
    vi.stubGlobal("confirm", vi.fn(() => true));
    fireEvent.click(button);
    await waitFor(() => expect(calls.some((c) => c.method === "DELETE" && c.url === "/v1/notebooks/nb1/sources/s1")).toBe(true));
  });

  it("adds a link, pasted text, a file, a path on Lucina's computer or a Keep note", async () => {
    stubWide();
    const { calls } = serve();
    await open();
    const add = async () => {
      fireEvent.click(document.querySelector<HTMLButtonElement>(".pg-cta")!);
      return screen.findByRole("dialog", { name: "Add source" });
    };
    // A link: the button waits for a real URL.
    let dialog = await add();
    const submit = within(dialog).getByRole("button", { name: "Add" });
    expect(submit).toBeDisabled();
    fireEvent.input(within(dialog).getByLabelText("Link"), { target: { value: "https://www.youtube.com/watch?v=abc" } });
    expect(submit).toBeEnabled();
    fireEvent.click(submit);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(calls.find((c) => c.method === "POST" && c.path === "/v1/notebooks/nb1/sources")!.body).toEqual({ kind: "url", url: "https://www.youtube.com/watch?v=abc" });
    // The notebook reloaded to show it.
    expect(calls.filter((c) => c.method === "GET" && c.path === "/v1/notebooks/nb1").length).toBeGreaterThan(1);

    // Pasted text.
    dialog = await add();
    fireEvent.click(within(dialog).getByRole("button", { name: "Paste text" }));
    fireEvent.input(within(dialog).getByLabelText("Title (optional)"), { target: { value: "Memo" } });
    fireEvent.input(within(dialog).getByLabelText("Text"), { target: { value: "Remember the exam." } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Add" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(calls.filter((c) => c.method === "POST").at(-1)!.body).toEqual({ kind: "text", title: "Memo", text: "Remember the exam." });

    // A file goes up as multipart to /sources/upload.
    dialog = await add();
    fireEvent.click(within(dialog).getByRole("button", { name: "Upload" }));
    const input = dialog.querySelector<HTMLInputElement>('input[type="file"]')!;
    fireEvent.change(input, { target: { files: [new File(["hello"], "notes.txt", { type: "text/plain" })] } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Add" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    const upload = calls.find((c) => c.path === "/v1/notebooks/nb1/sources/upload")!;
    expect(upload.headers["content-type"]).toMatch(/^multipart\/form-data/);
    expect(await upload.form!.text()).toContain('filename="notes.txt"');

    // A path in Lucina's computer must be absolute.
    dialog = await add();
    fireEvent.click(within(dialog).getByRole("button", { name: "Lucina's computer" }));
    const path = within(dialog).getByLabelText("Path");
    fireEvent.input(path, { target: { value: "Documents/a.pdf" } });
    expect(within(dialog).getByRole("button", { name: "Add" })).toBeDisabled();
    fireEvent.input(path, { target: { value: "/home/cua/Documents/a.pdf" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Add" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(calls.filter((c) => c.method === "POST" && c.path.endsWith("/sources")).at(-1)!.body).toEqual({ kind: "sandbox", path: "/home/cua/Documents/a.pdf" });

    // A Keep note, chosen from the owner's notes.
    dialog = await add();
    fireEvent.click(within(dialog).getByRole("button", { name: "Keep note" }));
    fireEvent.click(await within(dialog).findByRole("radio", { name: /Gift ideas/ }));
    fireEvent.click(within(dialog).getByRole("button", { name: "Add" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(calls.filter((c) => c.method === "POST" && c.path.endsWith("/sources")).at(-1)!.body).toEqual({ kind: "note", slug: "gift-ideas" });
  });

  it("keeps the sheet open with the server's reason when a source is refused", async () => {
    stubWide();
    serve({ extra: (c) => (c.method === "POST" ? { status: 400, body: { error: { code: "bad_request", message: "url: that address can't be reached" } } } : undefined) });
    await open();
    fireEvent.click(document.querySelector<HTMLButtonElement>(".pg-cta")!);
    const dialog = await screen.findByRole("dialog");
    fireEvent.input(within(dialog).getByLabelText("Link"), { target: { value: "https://nope.example" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Add" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("that address can't be reached");
  });
});

describe("the source reader", () => {
  it("opens at the cited passage, with the insights, and makes new ones", async () => {
    stubWide();
    const { calls } = serve({
      extra: (c) => (c.method === "POST" && c.path === "/v1/notebooks/sources/s1/insights" ? { status: 202, body: insight({ id: "i2", title: "Key Insights", status: "running", text: "" }) } : undefined),
    });
    await open();
    fireEvent.click(await within(col("Chat")).findByRole("button", { name: "Citation 1: RM Class 2.pdf" }));
    const dialog = await screen.findByRole("dialog", { name: "RM Class 2.pdf" });
    await within(dialog).findByText("Reliability is about consistency.");
    expect(calls.some((c) => c.path === "/v1/notebooks/sources/s1/chunks" && c.query.get("around") === "3")).toBe(true);
    const current = dialog.querySelector('[data-current]')!;
    expect(current).toHaveAttribute("data-ordinal", "3");
    expect(current).toHaveTextContent("p. 7");
    // The insight already there, and a transformation to run.
    expect(within(dialog).getByText("Dense Summary", { selector: "strong" })).toBeInTheDocument();
    expect(within(dialog).getByText("dense", { selector: "strong" })).toBeInTheDocument();
    fireEvent.change(within(dialog).getByLabelText("Transformation"), { target: { value: "t-key-insights" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Run" }));
    await waitFor(() => expect(calls.some((c) => c.method === "POST" && c.path === "/v1/notebooks/sources/s1/insights")).toBe(true));
    expect(calls.find((c) => c.method === "POST" && c.path === "/v1/notebooks/sources/s1/insights")!.body).toEqual({ transformation_id: "t-key-insights" });
    // The new insight is shown as in progress; saving the finished one as a note links it.
    expect(await within(dialog).findByText("Writing…")).toHaveAttribute("data-busy");
    fireEvent.click(within(dialog).getAllByRole("button", { name: "Save as note" })[0]!);
    expect(await within(dialog).findByRole("link", { name: /Saved to Keep/ })).toHaveAttribute("href", "/keep/note/saved-note");
    expect(calls.some((c) => c.path === "/v1/notebooks/insights/i1/note")).toBe(true);
  });

  it("opens from the Sources column at the start of the text, and closes with Escape", async () => {
    stubWide();
    const { calls } = serve();
    await open();
    fireEvent.click(within(col("Sources")).getByRole("button", { name: "RM Class 2.pdf" }));
    const dialog = await screen.findByRole("dialog");
    await within(dialog).findByText("Passage two text.");
    expect(calls.some((c) => c.path.endsWith("/chunks") && c.query.get("from") === "0" && c.query.get("limit") === "40")).toBe(true);
    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("says why a source has no text", async () => {
    stubWide();
    serve({
      nb: detail({ sources: [source({ id: "s1", status: "failed", error: "The PDF looks like a scan." })] }),
      extra: (c) => (c.path === "/v1/notebooks/sources/s1" ? { body: source({ status: "failed", error: "The PDF looks like a scan." }) } : undefined),
    });
    await open();
    fireEvent.click(within(col("Sources")).getByRole("button", { name: "RM Class 2.pdf" }));
    const dialog = await screen.findByRole("dialog");
    expect(await within(dialog).findAllByText("The PDF looks like a scan.")).not.toHaveLength(0);
    expect(within(dialog).getByRole("button", { name: "Run" })).toBeDisabled();
  });
});

describe("chat", () => {
  it("sends with DeepSeek by default, streams the reply, and shows its citations", async () => {
    stubWide();
    const exchange = {
      user: message({ id: "mu", role: "user", text: "And validity?" }),
      reply: message({ id: "mr", text: "Validity is meaning [source:s1-3].", citations: [citation()], model: "deepseek-flash" }),
    };
    const { calls } = serve({
      extra: (c) =>
        c.method === "POST" && c.path === "/v1/notebooks/chats/c1/messages"
          ? { sse: [sse("delta", { text: "Validity is " }), sse("delta", { text: "meaning" }), sse("message", exchange)] }
          : undefined,
    });
    await open();
    const chatCol = within(col("Chat"));
    await chatCol.findByText("What is reliability?");
    const box = chatCol.getByLabelText("Ask about your sources");
    expect(chatCol.getByRole("button", { name: "Send" })).toBeDisabled();
    fireEvent.input(box, { target: { value: "And validity?" } });
    fireEvent.click(chatCol.getByRole("button", { name: "Send" }));
    // Both the question and the stored reply end up in the log, with the reply's chip.
    await waitFor(() => expect(chatCol.getAllByText("Validity is meaning", { exact: false }).length).toBeGreaterThan(0));
    const post = calls.find((c) => c.method === "POST" && c.path.endsWith("/messages"))!;
    expect(post.body).toEqual({ text: "And validity?", model: "deepseek" });
    expect(post.headers.accept).toContain("text/event-stream");
    await waitFor(() => expect(chatCol.getAllByRole("button", { name: "Citation 1: RM Class 2.pdf" })).toHaveLength(2));
    expect((box as HTMLTextAreaElement).value).toBe("");
  });

  it("sends with the local model when it is chosen, and Enter sends while Shift+Enter does not", async () => {
    stubWide();
    const exchange = { user: message({ id: "mu", role: "user", text: "Q" }), reply: message({ id: "mr", text: "A" }) };
    const { calls } = serve({ extra: (c) => (c.method === "POST" && c.path.endsWith("/messages") ? { body: exchange } : undefined) });
    await open();
    const chatCol = within(col("Chat"));
    await chatCol.findByText("What is reliability?");
    fireEvent.change(chatCol.getByLabelText("Model"), { target: { value: "qwen" } });
    const box = chatCol.getByLabelText("Ask about your sources");
    fireEvent.input(box, { target: { value: "Q" } });
    fireEvent.keyDown(box, { key: "Enter", shiftKey: true });
    expect(calls.some((c) => c.method === "POST")).toBe(false);
    fireEvent.keyDown(box, { key: "Enter" });
    await waitFor(() => expect(calls.some((c) => c.method === "POST")).toBe(true));
    expect(calls.find((c) => c.method === "POST")!.body).toEqual({ text: "Q", model: "qwen" });
  });

  it("starts a chat on the first question when the notebook has none", async () => {
    stubWide();
    const exchange = { user: message({ id: "mu", role: "user", text: "First question" }), reply: message({ id: "mr", text: "An answer" }) };
    const { calls } = serve({
      nb: detail({ chats: [] }),
      extra: (c) => {
        if (c.method === "POST" && c.path === "/v1/notebooks/nb1/chats") return { status: 201, body: chat({ id: "c9", title: "First question", messages: 0 }) };
        if (c.method === "POST" && c.path === "/v1/notebooks/chats/c9/messages") return { body: exchange };
        if (c.path === "/v1/notebooks/nb1/chats") return { body: { chats: [chat({ id: "c9", title: "First question", messages: 2 })] } };
        return undefined;
      },
    });
    await open();
    const chatCol = within(col("Chat"));
    expect(await chatCol.findByText(/Ask something about your sources/)).toBeInTheDocument();
    fireEvent.input(chatCol.getByLabelText("Ask about your sources"), { target: { value: "First question" } });
    fireEvent.click(chatCol.getByRole("button", { name: "Send" }));
    expect(await chatCol.findByText("An answer")).toBeInTheDocument();
    expect(calls.find((c) => c.method === "POST" && c.path === "/v1/notebooks/nb1/chats")!.body).toEqual({ title: "First question" });
    expect(chatCol.getByRole("combobox", { name: "Chat" })).toHaveDisplayValue("First question");
  });

  it("keeps the question and says what went wrong when the reply fails", async () => {
    stubWide();
    serve({ extra: (c) => (c.method === "POST" && c.path.endsWith("/messages") ? { status: 503, body: { error: { code: "unavailable", message: "The model is busy." } } } : undefined) });
    await open();
    const chatCol = within(col("Chat"));
    await chatCol.findByText("What is reliability?");
    const box = chatCol.getByLabelText("Ask about your sources") as HTMLTextAreaElement;
    fireEvent.input(box, { target: { value: "Try this" } });
    fireEvent.click(chatCol.getByRole("button", { name: "Send" }));
    expect(await chatCol.findByRole("alert")).toHaveTextContent("The model is busy.");
    await waitFor(() => expect(box.value).toBe("Try this"));
  });

  it("opens the source at the passage from the citation list under a reply", async () => {
    stubWide();
    const { calls } = serve();
    await open();
    const chatCol = within(col("Chat"));
    const list = await chatCol.findByRole("list", { name: "Citations" });
    fireEvent.click(within(list).getByRole("button", { name: /RM Class 2\.pdf/ }));
    await screen.findByRole("dialog", { name: "RM Class 2.pdf" });
    expect(calls.some((c) => c.path.endsWith("/sources/s1/chunks") && c.query.get("around") === "3")).toBe(true);
  });

  it("saves a reply to a note and links to it, and the notebook reloads its notes", async () => {
    stubWide();
    const { calls, state } = serve({
      extra: (c) =>
        c.method === "POST" && c.path === "/v1/notebooks/chats/c1/messages/m1/note"
          ? { status: 201, body: noteLink({ slug: "answer-reliability", title: "Answer" }) }
          : undefined,
    });
    await open();
    const chatCol = within(col("Chat"));
    state.nb = detail({ notes: [noteLink(), noteLink({ slug: "answer-reliability", title: "Answer" })] });
    fireEvent.click(await chatCol.findByRole("button", { name: "Save to note" }));
    expect(await chatCol.findByRole("link", { name: /Saved to Keep/ })).toHaveAttribute("href", "/keep/note/answer-reliability");
    expect(calls.some((c) => c.path.endsWith("/messages/m1/note"))).toBe(true);
    expect(await within(col("Notes")).findByRole("link", { name: "Answer" })).toHaveAttribute("href", "/keep/note/answer-reliability");
  });

  it("renames and deletes a chat, and starts a new one", async () => {
    stubWide();
    vi.stubGlobal("prompt", vi.fn(() => "Final exam"));
    vi.stubGlobal("confirm", vi.fn(() => true));
    const { calls } = serve({ extra: (c) => (c.method === "PATCH" && c.path === "/v1/notebooks/chats/c1" ? { body: chat({ title: "Final exam" }) } : undefined) });
    await open();
    const chatCol = within(col("Chat"));
    await chatCol.findByText("What is reliability?");
    fireEvent.click(chatCol.getByRole("button", { name: "Rename chat" }));
    await waitFor(() => expect(chatCol.getByRole("combobox", { name: "Chat" })).toHaveDisplayValue("Final exam"));
    expect(calls.find((c) => c.method === "PATCH")!.body).toEqual({ title: "Final exam" });
    // New chat: the picker shows it and the log is empty until a question is sent.
    fireEvent.click(chatCol.getByRole("button", { name: "New chat" }));
    await waitFor(() => expect(chatCol.queryByText("What is reliability?")).toBeNull());
    expect(chatCol.getByRole("button", { name: "Delete chat" })).toBeDisabled();
    // Back to the old one, then delete it.
    fireEvent.change(chatCol.getByRole("combobox", { name: "Chat" }), { target: { value: "c1" } });
    await chatCol.findByText("What is reliability?");
    fireEvent.click(chatCol.getByRole("button", { name: "Delete chat" }));
    await waitFor(() => expect(calls.some((c) => c.method === "DELETE" && c.path === "/v1/notebooks/chats/c1")).toBe(true));
    await waitFor(() => expect(chatCol.queryByText("What is reliability?")).toBeNull());
  });
});

describe("notes", () => {
  it("writes a note into Keep, links an existing one and unlinks", async () => {
    stubWide();
    const { calls } = serve();
    await open();
    const notes = within(col("Notes"));
    fireEvent.click(notes.getByRole("button", { name: "Write note" }));
    let dialog = await screen.findByRole("dialog", { name: "Write note" });
    const save = within(dialog).getByRole("button", { name: "Save note" });
    expect(save).toBeDisabled();
    fireEvent.input(within(dialog).getByLabelText("Title"), { target: { value: "Exam tips" } });
    fireEvent.input(within(dialog).getByLabelText("Text"), { target: { value: "Sleep well." } });
    fireEvent.click(save);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(calls.find((c) => c.method === "POST" && c.path === "/v1/notebooks/nb1/notes")!.body).toEqual({ title: "Exam tips", text: "Sleep well." });

    fireEvent.click(notes.getByRole("button", { name: "Link note" }));
    dialog = await screen.findByRole("dialog", { name: "Link note" });
    // A note that is already linked is not offered again.
    await within(dialog).findByRole("radio", { name: /Gift ideas/ });
    expect(within(dialog).queryByRole("radio", { name: /RM summary/ })).toBeNull();
    fireEvent.click(within(dialog).getByRole("radio", { name: /Gift ideas/ }));
    fireEvent.click(within(dialog).getByRole("button", { name: "Link" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(calls.filter((c) => c.method === "POST" && c.path === "/v1/notebooks/nb1/notes").at(-1)!.body).toEqual({ slug: "gift-ideas" });

    fireEvent.click(notes.getByRole("button", { name: "Unlink RM summary" }));
    await waitFor(() => expect(calls.some((c) => c.method === "DELETE" && c.url === "/v1/notebooks/nb1/notes/rm-summary")).toBe(true));
  });

  it("explains an empty notes column", async () => {
    stubWide();
    serve({ nb: detail({ notes: [] }) });
    await open();
    expect(within(col("Notes")).getByText(/Notes you write here or link from Keep/)).toBeInTheDocument();
  });
});

describe("podcasts", () => {
  it("makes an episode from the profiles, a briefing and minutes", async () => {
    stubWide();
    const { calls } = serve();
    await open();
    fireEvent.click(screen.getByRole("button", { name: "Podcast" }));
    const dialog = await screen.findByRole("dialog", { name: "Podcast" });
    await within(dialog).findByRole("option", { name: "Deep dive" });
    fireEvent.change(within(dialog).getByLabelText("Format"), { target: { value: "deep-dive" } });
    fireEvent.input(within(dialog).getByLabelText("Briefing"), { target: { value: "Focus on the exam." } });
    fireEvent.input(within(dialog).getByLabelText("Minutes"), { target: { value: "12" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Create podcast" }));
    await waitFor(() => expect(calls.some((c) => c.method === "POST" && c.path === "/v1/notebooks/nb1/podcasts")).toBe(true));
    expect(calls.find((c) => c.method === "POST" && c.path.endsWith("/podcasts"))!.body).toEqual({
      episode_profile: "deep-dive",
      speaker_profile: "lucina-duo",
      briefing: "Focus on the exam.",
      minutes: 12,
    });
  });

  it("lists episodes with their state, an audio player for the ready one and its transcript", async () => {
    stubWide();
    serve({
      nb: detail({
        podcasts: [
          podcast(),
          podcast({ id: "p2", title: "Episode two", status: "voicing", audio: false, transcript: [], duration_s: null }),
          podcast({ id: "p3", title: "Episode three", status: "failed", audio: false, transcript: [], duration_s: null, error: "Speech is down." }),
        ],
      }),
    });
    await open();
    fireEvent.click(screen.getByRole("button", { name: "Podcast" }));
    const dialog = await screen.findByRole("dialog");
    const audio = dialog.querySelectorAll("audio");
    expect(audio).toHaveLength(1);
    expect(audio[0]).toHaveAttribute("src", "/v1/notebooks/podcasts/p1/audio");
    expect(within(dialog).getByText("Voicing…")).toHaveAttribute("data-busy");
    expect(within(dialog).getByText("Speech is down.")).toBeInTheDocument();
    expect(within(dialog).getByText("5:12", { exact: false })).toBeInTheDocument();
    const transcript = dialog.querySelector("details")!;
    expect(transcript).not.toHaveAttribute("open");
    expect(within(transcript as HTMLElement).getByText("Welcome.", { exact: false })).toBeInTheDocument();
  });
});

describe("search, settings and not found", () => {
  it("searches the notebook and opens a source hit at its passage", async () => {
    stubWide();
    const { calls } = serve({
      extra: (c) =>
        c.path === "/v1/notebooks/nb1/search"
          ? {
              body: {
                hits: [
                  { ref: "source:s1-3", kind: "source", source_id: "s1", title: "RM Class 2.pdf", excerpt: "consistency", score: 0.9 },
                  { ref: "note:rm-summary", kind: "note", slug: "rm-summary", title: "RM summary", excerpt: "note text", score: 0.5 },
                ],
              },
            }
          : undefined,
    });
    await open();
    fireEvent.click(screen.getByRole("button", { name: "Search this notebook" }));
    const dialog = await screen.findByRole("dialog", { name: "Search this notebook" });
    const box = within(dialog).getByLabelText("Search sources, notes and insights");
    fireEvent.input(box, { target: { value: "r" } });
    expect(within(dialog).getByText("Type at least two letters.")).toBeInTheDocument();
    fireEvent.input(box, { target: { value: "reliab" } });
    const hit = await within(dialog).findByRole("button", { name: /RM Class 2\.pdf/ });
    expect(calls.find((c) => c.path.endsWith("/search"))!.query.get("q")).toBe("reliab");
    expect(within(dialog).getByRole("link", { name: /RM summary/ })).toHaveAttribute("href", "/keep/note/rm-summary");
    fireEvent.click(hit);
    await waitFor(() => expect(calls.some((c) => c.path.endsWith("/sources/s1/chunks") && c.query.get("around") === "3")).toBe(true));
  });

  it("renames, archives and deletes (asking what to do with the sources)", async () => {
    stubWide();
    vi.stubGlobal("confirm", vi.fn(() => true));
    const { calls } = serve();
    await open();
    fireEvent.click(screen.getByRole("button", { name: "Notebook settings" }));
    let dialog = await screen.findByRole("dialog", { name: "Notebook settings" });
    fireEvent.input(within(dialog).getByLabelText("Name"), { target: { value: "RM" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(calls.find((c) => c.method === "PATCH")!.body).toEqual({ name: "RM", description: "Lecture notes" });

    fireEvent.click(screen.getByRole("button", { name: "Notebook settings" }));
    dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Archive" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(calls.filter((c) => c.method === "PATCH")[1]!.body).toEqual({ archived: true });

    fireEvent.click(screen.getByRole("button", { name: "Notebook settings" }));
    dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("radio", { name: "Delete sources no other notebook uses" }));
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete notebook" }));
    expect(await screen.findByText("The list")).toBeInTheDocument();
    expect(calls.find((c) => c.method === "DELETE")!.url).toBe("/v1/notebooks/nb1?sources=delete-exclusive");
  });

  it("does not delete when the question is declined", async () => {
    stubWide();
    vi.stubGlobal("confirm", vi.fn(() => false));
    const { calls } = serve();
    await open();
    fireEvent.click(screen.getByRole("button", { name: "Notebook settings" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete notebook" }));
    expect(calls.some((c) => c.method === "DELETE")).toBe(false);
  });

  it("says when the notebook does not exist, and offers a way to retry other errors", async () => {
    stubWide();
    stubNotebooks(() => ({ status: 404, body: { error: { code: "not_found", message: "Not found." } } }));
    renderAt("/notebooks/nb404", routes);
    expect(await screen.findByText("This notebook doesn't exist.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /Notebooks/ })).toHaveAttribute("href", "/notebooks");
  });

  it("shows an error card with Try again when loading fails", async () => {
    stubWide();
    let fail = true;
    serve({ extra: (c) => (fail && c.path === "/v1/notebooks/nb1" ? { status: 500, body: {} } : undefined) });
    renderAt("/notebooks/nb1", routes);
    expect(await screen.findByRole("alert")).toHaveTextContent("The server had a problem");
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByRole("heading", { level: 1, name: "Research Methods" });
  });
});

describe("following work in progress", () => {
  it("reloads every few seconds while a source is being read, and stops when it is ready", async () => {
    stubWide();
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let embedding = true;
    const { calls } = serve({
      extra: (c) =>
        c.path === "/v1/notebooks/nb1" && c.method === "GET"
          ? { body: detail({ sources: [source({ status: embedding ? "embedding" : "ready" })] }) }
          : undefined,
    });
    await open();
    const reads = () => calls.filter((c) => c.method === "GET" && c.path === "/v1/notebooks/nb1").length;
    expect(within(col("Sources")).getByText("Indexing…")).toBeInTheDocument();
    expect(reads()).toBe(1);
    await vi.advanceTimersByTimeAsync(3100);
    expect(reads()).toBe(2);
    embedding = false;
    await vi.advanceTimersByTimeAsync(3100);
    await waitFor(() => expect(within(col("Sources")).queryByText("Indexing…")).toBeNull());
    const settled = reads();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(reads()).toBe(settled);
  });

  it("does not poll a notebook that has nothing in progress", async () => {
    stubWide();
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const { calls } = serve();
    await open();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(calls.filter((c) => c.method === "GET" && c.path === "/v1/notebooks/nb1")).toHaveLength(1);
  });
});

describe("Notebook on a phone", () => {
  it("shows Sources, Notes and Chat as three tabs, one at a time", async () => {
    serve();
    await open();
    const nav = screen.getByRole("navigation", { name: "Research Methods" });
    expect(within(nav).getAllByRole("button").map((b) => b.textContent)).toEqual(["Sources", "Notes", "Chat"]);
    expect(within(nav).getByRole("button", { name: "Sources" })).toHaveAttribute("aria-current", "page");
    expect(screen.getByRole("region", { name: "Sources" })).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Notes" })).toBeNull();
    expect(screen.queryByRole("region", { name: "Chat" })).toBeNull();
    fireEvent.click(within(nav).getByRole("button", { name: "Notes" }));
    expect(await screen.findByRole("region", { name: "Notes" })).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Sources" })).toBeNull();
    fireEvent.click(within(nav).getByRole("button", { name: "Chat" }));
    const chatCol = within(await screen.findByRole("region", { name: "Chat" }));
    expect(await chatCol.findByText("What is reliability?")).toBeInTheDocument();
    expect(document.querySelector(".nb-cols--wide")).toBeNull();
  });
});
