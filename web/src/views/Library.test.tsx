import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import type { Book, Brief, Highlight } from "../api/library";
import { setLocale } from "../i18n";
import { stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
import LibraryView from "./Library";

const BOOK: Book = {
  id: "bk_0000000000000001",
  title: "A Room of One’s Own",
  authors: ["Virginia Woolf"],
  kind: "book",
  status: "unknown",
  progress: null,
  onKindle: { path: "documents/room.azw3", sizeBytes: 123, lastSeenAt: "2026-10-24T10:00:00Z" },
  owned: true,
  formats: ["azw3"],
  drm: "protected",
  updatedAt: "2026-10-24T10:00:00Z",
  isbn: null,
  doi: null,
  asin: "B123",
  fingerprints: [],
  sources: [{ kind: "kindle-usb", device: null }],
  removedFromKindleAt: null,
  mentionedOnly: false,
  statusSource: null,
  lastOpenedAt: null,
  tags: [],
  briefs: [],
  relatedNotes: [],
  memoryRefs: [],
  createdAt: "2026-10-24T10:00:00Z",
};
const HIGHLIGHT: Highlight = {
  id: "hl_1",
  bookId: BOOK.id,
  kind: "highlight",
  text: "A woman must have money and a room of her own.",
  note: "Think about a writing space.",
  location: { start: 12, end: 14 },
  page: "3",
  addedAt: null,
  addedAtRaw: "Friday, 23 October 2026 10:30",
  source: "kindle-clippings",
  provenance: { file: "My Clippings.txt", entryIndex: 0, device: null },
  dedupKey: "opaque:new:format",
};
const BRIEF: Brief = {
  id: "brief_1",
  kind: "extractive",
  text: "A woman must have money…",
  generatedAt: "2026-10-24T11:00:00Z",
  model: null,
  basedOn: [HIGHLIGHT.id],
};
function server(override: (c: Call) => { status?: number; body?: unknown } | undefined = () => undefined) {
  let book = { ...BOOK };
  return stubFetch((c) => {
    const o = override(c);
    if (o) return o;
    const url = new URL(c.url, "http://x");
    if (url.pathname === "/v1/library/books")
      return {
        body: {
          books: [
            { ...book, highlightCount: 1, lastHighlightAt: null },
            { ...book, id: "bk_dictionary", title: "Dictionary", kind: "dictionary", highlightCount: 0 },
          ].filter((b) => !url.searchParams.get("status") || b.status === url.searchParams.get("status")),
        },
      };
    if (url.pathname === "/v1/library/search")
      return {
        body: {
          results: [
            {
              bookId: BOOK.id,
              title: BOOK.title,
              authors: BOOK.authors,
              highlightId: HIGHLIGHT.id,
              snippet: "A room of her own",
              location: HIGHLIGHT.location,
              page: "3",
              source: "kindle-clippings",
            },
          ],
        },
      };
    if (url.pathname === `/v1/library/books/${BOOK.id}`) {
      if (c.method === "PATCH") book = { ...book, ...(c.body as object), updatedAt: "2026-10-24T11:00:00Z" };
      return { body: c.method === "PATCH" ? book : { book, highlights: [HIGHLIGHT], sessions: [] } };
    }
    if (url.pathname.endsWith("/brief")) {
      book = { ...book, briefs: [BRIEF] };
      return { body: BRIEF };
    }
    if (url.pathname === "/v1/library/stats")
      return {
        body: {
          byStatus: { unknown: 1 },
          byFormat: { azw3: 1 },
          byDrm: { protected: 1 },
          highlights: 1,
          booksOnKindle: 1,
          lastImport: null,
        },
      };
    if (url.pathname === "/v1/library/sync")
      return { body: { imports: [{ time: "2026-10-24T10:00:00Z", source: "kindle-usb" }], books: 1, highlights: 1 } };
    if (url.pathname === "/v1/library/recent")
      return {
        body: {
          highlights: [HIGHLIGHT],
          changes: [],
          sessions: [],
          readingSessions: "unavailable_on_stock_kindle",
          unknownDateHighlightsExcluded: true,
          kindleDatesHaveNoTimezone: true,
        },
      };
    return { status: 404 };
  });
}
const renderLibrary = (path = "/library") => renderAt(path, () => <Route path="/library/:id?" component={LibraryView} />);

describe("Library", () => {
  it("lists books, hides dictionaries, and renders unknown progress without inventing a percent", async () => {
    server();
    renderLibrary();
    expect(await screen.findByRole("heading", { name: BOOK.title })).toBeInTheDocument();
    expect(screen.getByText("Virginia Woolf")).toBeInTheDocument();
    expect(screen.getByText("Progress: —")).toBeInTheDocument();
    expect(screen.getByText("1 highlight")).toBeInTheDocument();
    expect(screen.getByText("Protected")).toBeInTheDocument();
    expect(screen.queryByText("Dictionary")).not.toBeInTheDocument();
    expect(screen.queryByText(/0%/)).not.toBeInTheDocument();
    expect(await screen.findByText(/Reading sessions are unavailable on a stock Kindle/)).toBeInTheDocument();
    expect(screen.getByText("Highlights in the last 7 days: 1")).toBeInTheDocument();
    expect(screen.getByText("Highlights in the last 30 days: 1")).toBeInTheDocument();
  });
  it("sends status and Kindle filters and debounces search through the search route", async () => {
    const calls = server();
    renderLibrary();
    await screen.findByRole("heading", { name: BOOK.title });
    fireEvent.click(screen.getByRole("button", { name: "Reading" }));
    await waitFor(() => expect(calls.some((c) => c.url.includes("status=reading"))).toBe(true));
    expect(await screen.findByText("No books match these filters.")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "All" }));
    fireEvent.click(screen.getByRole("button", { name: "On Kindle" }));
    await waitFor(() => expect(calls.some((c) => c.url.includes("onKindle=true"))).toBe(true));
    fireEvent.input(screen.getByRole("searchbox"), { target: { value: "room" } });
    expect(calls.some((c) => c.url.startsWith("/v1/library/search"))).toBe(false);
    expect(await screen.findByText("A room of her own")).toBeInTheDocument();
    expect(calls.some((c) => c.url === "/v1/library/search?q=room&limit=500")).toBe(true);
  });
  it("shows metadata, unknown progress, highlights with notes and local date, and related notes", async () => {
    server();
    renderLibrary(`/library/${BOOK.id}`);
    expect(await screen.findByText("Unknown — the stock Kindle does not expose progress over USB")).toBeInTheDocument();
    expect(screen.queryByText(/0%/)).not.toBeInTheDocument();
    expect(screen.getByText("Owned")).toBeInTheDocument();
    expect(screen.getByText("On Kindle")).toBeInTheDocument();
    expect(screen.getByText("Location 12–14")).toBeInTheDocument();
    expect(screen.getByText("Page 3")).toBeInTheDocument();
    expect(screen.getByText(HIGHLIGHT.addedAtRaw)).toBeInTheDocument();
    expect(screen.getByText(HIGHLIGHT.note!)).toBeInTheDocument();
    expect(screen.getByText("Obsidian is not connected yet")).toBeInTheDocument();
  });
  it("patches owner status and tags with the read version and CSRF header", async () => {
    const calls = server();
    renderLibrary(`/library/${BOOK.id}`);
    const control = await screen.findByLabelText("Reading status");
    fireEvent.change(control, { target: { value: "reading" } });
    await waitFor(() => expect(control).not.toBeDisabled());
    await waitFor(() => expect(calls.some((c) => c.method === "PATCH")).toBe(true));
    expect(calls.find((c) => c.method === "PATCH")!).toMatchObject({
      body: { status: "reading", baseUpdatedAt: BOOK.updatedAt },
      headers: { "x-secretary-csrf": "1", "x-secretary-client": "web" },
    });
    fireEvent.input(screen.getByLabelText("Tags, separated by commas"), { target: { value: "Writing, #essay" } });
    fireEvent.click(screen.getByRole("button", { name: "Save tags" }));
    await waitFor(() => expect(calls.filter((c) => c.method === "PATCH")).toHaveLength(2));
    expect(calls.filter((c) => c.method === "PATCH")[1]!.body).toEqual({
      tags: ["writing", "essay"],
      baseUpdatedAt: "2026-10-24T11:00:00Z",
    });
  });
  it("reloads on a 409 and tells the owner before another edit", async () => {
    let updated = false;
    const calls = server((c) => {
      if (c.method === "PATCH") {
        updated = true;
        return { status: 409, body: { error: { code: "conflict", message: "changed" } } };
      }
      if (updated && c.url === `/v1/library/books/${BOOK.id}`)
        return { body: { book: { ...BOOK, status: "completed", updatedAt: "new-version" }, highlights: [], sessions: [] } };
      return undefined;
    });
    renderLibrary(`/library/${BOOK.id}`);
    fireEvent.change(await screen.findByLabelText("Reading status"), { target: { value: "reading" } });
    expect(await screen.findByText(/This book changed while you were editing/)).toBeInTheDocument();
    expect(screen.getByLabelText("Reading status")).toHaveValue("completed");
    expect(calls.filter((c) => c.method === "GET" && c.url === `/v1/library/books/${BOOK.id}`)).toHaveLength(2);
  });
  it("makes an extractive brief and labels it as generated from highlights", async () => {
    const calls = server();
    renderLibrary(`/library/${BOOK.id}`);
    fireEvent.click(await screen.findByRole("button", { name: "Make brief" }));
    expect(await screen.findByText("Generated from your highlights")).toBeInTheDocument();
    expect(screen.getByText(BRIEF.text)).toBeInTheDocument();
    expect(calls.find((c) => c.method === "POST")!).toMatchObject({
      url: `/v1/library/books/${BOOK.id}/brief`,
      headers: { "x-secretary-csrf": "1" },
    });
  });
  it("explains USB import when the library is empty", async () => {
    server((c) => (c.url.startsWith("/v1/library/books?") ? { body: { books: [] } } : undefined));
    renderLibrary();
    expect(await screen.findByText("Your library is empty")).toBeInTheDocument();
    expect(screen.getByText("lucina-ink import /Volumes/Kindle --server … --token-file …")).toBeInTheDocument();
  });
  it("translates the Library into Italian and Danish", async () => {
    server();
    setLocale("it");
    renderLibrary();
    expect(await screen.findByRole("heading", { name: "Biblioteca" })).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Cerca nella biblioteca")).toBeInTheDocument();
    setLocale("da");
    expect(screen.getByRole("heading", { name: "Bibliotek" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "På Kindle" })).toBeInTheDocument();
  });
  it("renders a recoverable list error and retries", async () => {
    let fail = true;
    server((c) => (fail && c.url.startsWith("/v1/library/books?") ? { status: 503 } : undefined));
    renderLibrary();
    expect(await screen.findByText("Could not load or save the library. Please try again.")).toBeInTheDocument();
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("heading", { name: BOOK.title })).toBeInTheDocument();
  });
  it("renders a recoverable detail error and retries", async () => {
    let fail = true;
    server((c) => (fail && c.url === `/v1/library/books/${BOOK.id}` ? { status: 404 } : undefined));
    renderLibrary(`/library/${BOOK.id}`);
    expect(await screen.findByText("Could not load or save the library. Please try again.")).toBeInTheDocument();
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("heading", { name: BOOK.title })).toBeInTheDocument();
  });
  it("supports absent kind fields and distinguishes known zero progress from unknown", async () => {
    server((c) =>
      c.url === `/v1/library/books/${BOOK.id}`
        ? {
            body: {
              book: { ...BOOK, kind: undefined, progress: { percent: 0, location: null, page: null, source: "owner", at: BOOK.updatedAt } },
              highlights: [],
              sessions: [],
            },
          }
        : undefined,
    );
    renderLibrary(`/library/${BOOK.id}`);
    expect(await screen.findByText("0%")).toBeInTheDocument();
    expect(screen.queryByText(/does not expose progress over USB/)).not.toBeInTheDocument();
  });
  it("shows clippings-only presence and the removal date independently", async () => {
    server((c) =>
      c.url === `/v1/library/books/${BOOK.id}`
        ? {
            body: {
              book: { ...BOOK, owned: false, onKindle: null, removedFromKindleAt: "2026-10-24T10:00:00Z" },
              highlights: [HIGHLIGHT],
              sessions: [],
            },
          }
        : undefined,
    );
    renderLibrary(`/library/${BOOK.id}`);
    expect(await screen.findByText("Clippings only")).toBeInTheDocument();
    expect(screen.getByText(/Removed from Kindle:/)).toBeInTheDocument();
    expect(screen.queryByText("Owned")).not.toBeInTheDocument();
    expect(screen.queryByText("On Kindle")).not.toBeInTheDocument();
  });
});
