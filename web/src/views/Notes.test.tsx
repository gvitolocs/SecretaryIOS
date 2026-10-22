import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { setLocale } from "../i18n";
import { stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
import NotesView from "./Notes";

const BRIEFING = {
  id: "n_00000000000000000001",
  slug: "craftit-meeting",
  title: "CRAFT-IT4SD — meeting with Marianne & Jonas",
  content: "# Briefing\n\nMarianne offers **paid hours**. See [the site](https://craft-it4sd.eu/) and [bad](javascript:alert(1)).\n\n- Excel survey\n- Resources",
  tags: ["work", "aarhus"],
  source: "chatgpt",
  pinned: true,
  createdAt: "2026-10-22T11:30:00.000Z",
  updatedAt: "2026-10-22T11:31:00.000Z",
};
const IDEAS = {
  id: "n_00000000000000000002",
  slug: "gift-ideas",
  title: "Gift ideas",
  content: "- a book",
  tags: [],
  source: "lucina",
  pinned: false,
  createdAt: "2026-10-20T08:00:00.000Z",
  updatedAt: "2026-10-20T08:00:00.000Z",
};

const summary = ({ content, ...n }: typeof BRIEFING) => ({ ...n, size: content.length, excerpt: content.slice(0, 60) });

function server(overrides: (c: Call) => { status?: number; body?: unknown } | undefined = () => undefined) {
  return stubFetch((c) => {
    const o = overrides(c);
    if (o) return o;
    const url = new URL(c.url, "http://x");
    if (c.method === "GET" && url.pathname === "/v1/notes") {
      const q = (url.searchParams.get("q") ?? "").toLowerCase();
      const notes = [BRIEFING, IDEAS].filter((n) => !q || `${n.title} ${n.content}`.toLowerCase().includes(q)).map(summary);
      return { body: { notes } };
    }
    if (c.method === "GET" && url.pathname === "/v1/notes/craftit-meeting") return { body: BRIEFING };
    if (c.method === "GET" && url.pathname === "/v1/notes/gift-ideas") return { body: IDEAS };
    if (c.method === "GET" && url.pathname === "/v1/notes/trash") return { body: { notes: [], days: 30 } };
    return { status: 404, body: { error: { code: "note_not_found", message: "no note" } } };
  });
}

const renderNotes = (path: string) => renderAt(path, () => <Route path="/notes/:key?" component={NotesView} />);

describe("Notes", () => {
  it("lists pinned notes first with their source, and searches", async () => {
    const calls = server();
    renderNotes("/notes");
    expect(await screen.findByText("CRAFT-IT4SD — meeting with Marianne & Jonas")).toBeInTheDocument();
    expect(screen.getByText("Gift ideas")).toBeInTheDocument();
    expect(screen.getByText("Pinned")).toBeInTheDocument();
    expect(screen.getByText("ChatGPT")).toBeInTheDocument();
    expect(screen.getByText("Lucina")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "#aarhus" })).toBeInTheDocument();
    // The list never downloads note contents.
    expect(calls.filter((c) => c.url.startsWith("/v1/notes/"))).toHaveLength(0);

    fireEvent.input(screen.getByPlaceholderText("Search notes"), { target: { value: "book" } });
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/notes?q=book")).toBe(true));
    await waitFor(() => expect(screen.queryByText("CRAFT-IT4SD — meeting with Marianne & Jonas")).not.toBeInTheDocument());
    fireEvent.input(screen.getByPlaceholderText("Search notes"), { target: { value: "zzz" } });
    expect(await screen.findByText("Nothing matches “zzz”.")).toBeInTheDocument();
  });

  it("renders a note as safe markdown with its source and name", async () => {
    server();
    const { container } = renderNotes("/notes/craftit-meeting");
    expect(await screen.findByRole("heading", { level: 1, name: "CRAFT-IT4SD — meeting with Marianne & Jonas" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Briefing" })).toBeInTheDocument();
    expect(screen.getByText("paid hours").tagName).toBe("STRONG");
    const site = screen.getByRole("link", { name: "the site" });
    expect(site).toHaveAttribute("href", "https://craft-it4sd.eu/");
    expect(site).toHaveAttribute("rel", "noopener noreferrer nofollow");
    expect(screen.queryByRole("link", { name: "bad" })).not.toBeInTheDocument();
    expect(container.innerHTML).not.toContain("javascript:");
    expect(screen.getByText("Name: craftit-meeting")).toBeInTheDocument();
    expect(screen.getAllByText("ChatGPT").length).toBeGreaterThan(0);
    expect(screen.getByText(/what's in the note craftit-meeting/)).toBeInTheDocument();
  });

  it("creates a note with the CSRF header and opens it", async () => {
    const calls = server((c) => {
      if (c.method === "POST" && c.url === "/v1/notes") {
        return { status: 201, body: { ...IDEAS, id: "n_00000000000000000003", slug: "trip", title: "Trip", content: "Rome" } };
      }
      if (c.method === "GET" && c.url === "/v1/notes/trip") return { body: { ...IDEAS, slug: "trip", title: "Trip", content: "Rome" } };
      return undefined;
    });
    renderNotes("/notes/new");
    fireEvent.click(await screen.findByRole("button", { name: "Save" }));
    expect(await screen.findByText("Give the note a title.")).toBeInTheDocument();
    fireEvent.input(screen.getByLabelText("Title"), { target: { value: "Trip" } });
    fireEvent.input(screen.getByLabelText("Tags, separated by commas"), { target: { value: "Travel, #rome" } });
    fireEvent.input(screen.getByLabelText("Text (Markdown)"), { target: { value: "Rome" } });
    fireEvent.click(screen.getByRole("tab", { name: "Preview" }));
    expect(await screen.findByText("Rome")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(calls.some((c) => c.method === "POST")).toBe(true));
    const post = calls.find((c) => c.method === "POST")!;
    expect(post.headers["x-secretary-csrf"]).toBe("1");
    expect(post.headers["x-secretary-client"]).toBe("web");
    expect(post.body).toEqual({ title: "Trip", content: "Rome", tags: ["travel", "rome"], pinned: false });
    expect(await screen.findByRole("heading", { level: 1, name: "Trip" })).toBeInTheDocument();
  });

  it("edits with the version it read, and keeps the text on a conflict", async () => {
    let conflict = true;
    const calls = server((c) => {
      if (c.method === "PATCH") {
        if (conflict) return { status: 409, body: { error: { code: "conflict", message: "changed" } } };
        return { body: { ...IDEAS, content: (c.body as { content: string }).content, updatedAt: "2026-10-22T12:00:00.000Z" } };
      }
      return undefined;
    });
    renderNotes("/notes/gift-ideas");
    fireEvent.click(await screen.findByRole("button", { name: "Edit" }));
    fireEvent.input(screen.getByLabelText("Text (Markdown)"), { target: { value: "- a book\n- a scarf" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText(/This note changed while you were editing/)).toBeInTheDocument();
    expect(screen.getByLabelText("Text (Markdown)")).toHaveValue("- a book\n- a scarf");
    const patch = calls.find((c) => c.method === "PATCH")!;
    expect(patch.url).toBe(`/v1/notes/${IDEAS.id}`);
    expect(patch.body).toMatchObject({ content: "- a book\n- a scarf", baseUpdatedAt: IDEAS.updatedAt });
    conflict = false;
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText("a scarf")).toBeInTheDocument();
  });

  it("moves a note to the trash after a confirmation and can undo it", async () => {
    const calls = server((c) => {
      if (c.method === "DELETE") return { body: { deleted: true, id: IDEAS.id } };
      if (c.method === "POST" && c.url.endsWith("/restore")) return { body: IDEAS };
      return undefined;
    });
    renderNotes("/notes/gift-ideas");
    fireEvent.click(await screen.findByRole("button", { name: "Delete" }));
    expect(screen.getByText("Move “Gift ideas” to the trash? You can restore it for 30 days.")).toBeInTheDocument();
    expect(calls.some((c) => c.method === "DELETE")).toBe(false);
    const dialog = screen.getByRole("alertdialog");
    fireEvent.click(dialog.querySelector(".btn--danger")!);
    expect(await screen.findByText("“Gift ideas” is in the trash.")).toBeInTheDocument();
    expect(calls.find((c) => c.method === "DELETE")!.url).toBe(`/v1/notes/${IDEAS.id}`);
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(calls.some((c) => c.url === `/v1/notes/trash/${IDEAS.id}/restore`)).toBe(true));
  });

  it("speaks Italian and Danish", async () => {
    server();
    setLocale("it");
    renderNotes("/notes");
    expect(await screen.findByRole("heading", { level: 1, name: "Note" })).toBeInTheDocument();
    expect(await screen.findByText("In evidenza")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Nuova nota" })).toBeInTheDocument();
    setLocale("da");
    expect(await screen.findByRole("heading", { level: 1, name: "Noter" })).toBeInTheDocument();
  });
});
