import { createMemoryHistory, MemoryRouter, Route } from "@solidjs/router";
import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { KeepNote } from "../api/keep";
import { dismissSnackbar, keep } from "../state/keep";
import { applyInput } from "../state/keep";
import { clearSelection, setSearch, setSearchOpen } from "../state/keepUi";
import { emptySearch } from "../lib/keepFilter";
import { stubFetch, type Call } from "../test/fetch";
import { note } from "../test/keepFixtures";
import { renderAt } from "../test/router";
import KeepView from "./Keep";

const NOW = "2026-10-27T10:00:00.000Z";
const seed = (): KeepNote[] => [
  note({ id: "n_pin", title: "Revision plan", content: "Split into **blocks**", color: "teal", pinned: true, tags: ["godlevel"], position: 900, rev: 5 }),
  note({ id: "n_text", slug: "n-text-slug", title: "Active recall", content: "Test yourself", tags: ["godlevel"], position: 800, rev: 4 }),
  note({
    id: "n_list",
    title: "Groceries",
    items: [
      { id: "i1", text: "Oats", checked: false, indent: 0 },
      { id: "i2", text: "Lemons", checked: true, indent: 0 },
      { id: "i3", text: "Spinach", checked: false, indent: 0 },
    ],
    position: 700,
    rev: 3,
  }),
  note({ id: "n_arch", title: "Old idea", archived: true, position: 600, rev: 2 }),
  note({ id: "n_trash", title: "Throwaway", deletedAt: "2026-10-25T10:00:00.000Z", position: 500, rev: 6 }),
];
const LABELS = [
  { tag: "godlevel", name: "GodLevel", count: 2 },
  { tag: "chill", name: "Chill", count: 0 },
];

let calls: Call[] = [];
function server(notes: KeepNote[] = seed()) {
  let synced = false;
  calls = stubFetch((c) => {
    const url = new URL(c.url, "http://x");
    if (c.method === "GET" && url.pathname === "/v1/notes/changes") {
      const body = { notes: synced ? [] : notes, removed: [], cursor: String(synced ? 9 : 6), hasMore: false };
      synced = true;
      return { body };
    }
    if (c.method === "GET" && url.pathname === "/v1/notes/labels") return { body: { labels: LABELS } };
    if (c.method === "POST" && url.pathname === "/v1/notes") {
      const b = c.body as Record<string, unknown>;
      return { status: 201, body: { ...applyInput(note({ id: "n_created", rev: 10 }), b as never, NOW), title: String(b.title ?? "") } };
    }
    const m = /^\/v1\/notes\/([^/]+)$/.exec(url.pathname);
    if (c.method === "PATCH" && m) {
      const base = notes.find((n) => n.id === m[1]) ?? note({ id: m[1]! });
      return { body: applyInput({ ...base, rev: 20 }, c.body as never, NOW) };
    }
    if (c.method === "POST" && url.pathname === "/v1/notes/batch") return { body: { notes: [] } };
    if (c.method === "DELETE") return { body: { ok: true } };
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
}

function renderKeepWithHistory(path: string) {
  const history = createMemoryHistory();
  history.set({ value: path, scroll: false, replace: true });
  const r = render(() => (
    <MemoryRouter history={history}>
      <Route path="/keep/*rest" component={KeepView} />
    </MemoryRouter>
  ));
  return { ...r, history };
}

/** Like server(), and GET /v1/notes/<slug> answers with `extra` (the changes feed does not list it). */
function stubGetBySlug(extra: KeepNote) {
  const notes = seed();
  let synced = false;
  calls = stubFetch((c) => {
    const url = new URL(c.url, "http://x");
    if (c.method === "GET" && url.pathname === "/v1/notes/changes") {
      const body = { notes: synced ? [] : notes, removed: [], cursor: "6", hasMore: false };
      synced = true;
      return { body };
    }
    if (c.method === "GET" && url.pathname === "/v1/notes/labels") return { body: { labels: LABELS } };
    if (c.method === "GET" && url.pathname === `/v1/notes/${extra.slug}`) return { body: extra };
    return { status: 404, body: { error: { code: "note_not_found", message: "no" } } };
  });
}

const renderKeep = (path = "/keep") => renderAt(path, () => <Route path="/keep/*rest" component={KeepView} />);
const card = (title: string) => screen.getByText(title, { selector: ".kp-card-title" }).closest("article") as HTMLElement;
const titles = () => [...document.querySelectorAll(".kp-card-title")].map((e) => e.textContent);

beforeEach(async () => {
  await keep.reset();
  setSearch(emptySearch());
  setSearchOpen(false);
  clearSelection();
  dismissSnackbar();
});
afterEach(() => dismissSnackbar());

describe("Keep: the grid", () => {
  it("shows Pinned and Others, the checklist with unchecked items first, and the labels in the nav", async () => {
    server();
    renderKeep();
    expect(await screen.findByText("Revision plan", { selector: ".kp-card-title" })).toBeInTheDocument();
    expect(screen.getByText("Pinned")).toBeInTheDocument();
    expect(screen.getByText("Others")).toBeInTheDocument();
    // Pinned first, then by position; the archive and the trash are not in Notes.
    expect(titles()).toEqual(["Revision plan", "Active recall", "Groceries"]);
    const groceries = within(card("Groceries"));
    expect(groceries.getByText("Oats")).toBeInTheDocument();
    expect(groceries.getByText("Spinach")).toBeInTheDocument();
    expect(groceries.queryByText("Lemons")).toBeNull();
    expect(groceries.getByText("+ 1 checked item")).toBeInTheDocument();
    // Rich text on the card: bold, safely.
    expect(within(card("Revision plan")).getByText("blocks").tagName).toBe("STRONG");
    // The nav: Notes, Reminders, labels (alphabetical, display case), Edit labels, Archive, Trash.
    const nav = within(screen.getByRole("navigation", { name: "Main menu" }));
    await waitFor(() => expect(nav.getByRole("link", { name: "GodLevel" })).toBeInTheDocument());
    expect(nav.getAllByRole("link").map((a) => a.textContent)).toEqual(["Notes", "Reminders", "Chill", "GodLevel", "Archive", "Trash"]);
    expect(nav.getByRole("link", { name: "Notes" })).toHaveAttribute("aria-current", "page");
  });

  it("asks for the cache's changes and for the label registry", async () => {
    server();
    renderKeep();
    await screen.findByText("Revision plan", { selector: ".kp-card-title" });
    expect(calls.some((c) => c.method === "GET" && c.url === "/v1/notes/changes?since=0&limit=200")).toBe(true);
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/notes/labels")).toBe(true));
  });

  it("shows the archive and the trash on their own routes", async () => {
    server();
    renderKeep("/keep/archive");
    expect(await screen.findByText("Old idea", { selector: ".kp-card-title" })).toBeInTheDocument();
    expect(titles()).toEqual(["Old idea"]);
  });

  it("shows a label's notes at /keep/label/<name>", async () => {
    server();
    renderKeep("/keep/label/GodLevel");
    await screen.findByText("Revision plan", { selector: ".kp-card-title" });
    expect(titles()).toEqual(["Revision plan", "Active recall"]);
    expect(await screen.findByRole("link", { name: "GodLevel" })).toHaveAttribute("aria-current", "page");
  });

  it("tells an empty view so", async () => {
    server([]);
    renderKeep("/keep/reminders");
    expect(await screen.findByText("Notes with upcoming reminders appear here")).toBeInTheDocument();
  });
});

describe("Keep: search", () => {
  it("opens the filter page on focus, and filters live as you type", async () => {
    server();
    renderKeep();
    await screen.findByText("Revision plan", { selector: ".kp-card-title" });
    const box = screen.getByRole("searchbox", { name: "Search" });
    fireEvent.focus(box);
    const page = screen.getByRole("region", { name: "Search" });
    expect(within(page).getByText("Types")).toBeInTheDocument();
    for (const t of ["Reminders", "Lists", "Images", "Audio", "URLs"]) expect(within(page).getByRole("button", { name: t })).toBeInTheDocument();
    expect(within(page).getByText("Colors")).toBeInTheDocument();
    expect(await within(page).findByRole("button", { name: "GodLevel" })).toBeInTheDocument();

    fireEvent.input(box, { target: { value: "recall" } });
    await waitFor(() => expect(titles()).toEqual(["Active recall"]));
    fireEvent.input(box, { target: { value: "lemons" } }); // an item text, checked
    await waitFor(() => expect(titles()).toEqual(["Groceries"]));
    fireEvent.input(box, { target: { value: "zzz" } });
    expect(await screen.findByText("No matching notes found.")).toBeInTheDocument();
  });

  it("filters by a type chip and a label chip", async () => {
    server();
    renderKeep();
    await screen.findByText("Revision plan", { selector: ".kp-card-title" });
    fireEvent.focus(screen.getByRole("searchbox", { name: "Search" }));
    fireEvent.click(screen.getByRole("button", { name: "Lists" }));
    await waitFor(() => expect(titles()).toEqual(["Groceries"]));
    expect(document.querySelector('[data-chip="type"]')).toHaveTextContent("Lists");
    // Removing the last filter with the search box still focused brings the filter page back.
    fireEvent.click(within(document.querySelector('[data-chip="type"]') as HTMLElement).getByRole("button"));
    expect(await screen.findByRole("region", { name: "Search" })).toBeInTheDocument();
    expect(titles()).toEqual([]);
  });
});

describe("Keep: the composer", () => {
  it("saves a titled note on Close with the options chosen", async () => {
    server();
    renderKeep();
    await screen.findByText("Revision plan", { selector: ".kp-card-title" });
    fireEvent.click(screen.getByRole("button", { name: "Take a note…" }));
    fireEvent.input(screen.getByRole("textbox", { name: "Title" }), { target: { value: "Trip ideas" } });
    const body = screen.getByRole("textbox", { name: "Take a note…" });
    body.textContent = "Book the train";
    fireEvent.input(body);
    fireEvent.click(screen.getAllByRole("button", { name: "Pin note" })[0]!);
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    await waitFor(() => expect(calls.some((c) => c.method === "POST" && c.url === "/v1/notes")).toBe(true));
    const post = calls.find((c) => c.method === "POST" && c.url === "/v1/notes")!;
    expect(post.body).toMatchObject({ title: "Trip ideas", content: "Book the train", pinned: true, color: "default", archived: false });
    // The new note is on screen straight away.
    expect(titles()).toContain("Trip ideas");
  });

  it("saves a list made with 'New list'", async () => {
    server();
    renderKeep();
    await screen.findByText("Revision plan", { selector: ".kp-card-title" });
    fireEvent.click(screen.getByRole("button", { name: "New list" }));
    fireEvent.input(screen.getByRole("textbox", { name: "Title" }), { target: { value: "Todo" } });
    const item = screen.getByRole("textbox", { name: "List item" });
    fireEvent.input(item, { target: { value: "call mum" } });
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    await waitFor(() => expect(calls.some((c) => c.method === "POST" && c.url === "/v1/notes")).toBe(true));
    const post = calls.find((c) => c.method === "POST" && c.url === "/v1/notes")!;
    expect((post.body as { items: { text: string }[] }).items.map((i) => i.text)).toEqual(["call mum"]);
  });

  it("discards an empty note without any request", async () => {
    server();
    renderKeep();
    await screen.findByText("Revision plan", { selector: ".kp-card-title" });
    const before = calls.length;
    fireEvent.click(screen.getByRole("button", { name: "Take a note…" }));
    const composer = screen.getByRole("group", { name: "New note" });
    fireEvent.click(within(composer).getByRole("button", { name: "Background options" }));
    fireEvent.click(await screen.findByRole("radio", { name: "Red" }));
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    await Promise.resolve();
    expect(calls.slice(before).some((c) => c.method === "POST")).toBe(false);
    expect(screen.getByRole("button", { name: "Take a note…" })).toBeInTheDocument();
  });
});

describe("Keep: actions on a card", () => {
  it("archives with a snackbar, and Undo brings it back", async () => {
    server();
    renderKeep();
    await screen.findByText("Revision plan", { selector: ".kp-card-title" });
    fireEvent.click(within(card("Active recall")).getByRole("button", { name: "Archive" }));
    expect(await screen.findByText("Note archived")).toBeInTheDocument();
    expect(titles()).not.toContain("Active recall");
    await waitFor(() => expect(calls.some((c) => c.method === "PATCH" && c.url === "/v1/notes/n_text")).toBe(true));
    expect(calls.find((c) => c.method === "PATCH")!.body).toEqual({ archived: true, pinned: false });
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(titles()).toContain("Active recall"));
    await waitFor(() => expect(calls.filter((c) => c.method === "PATCH")).toHaveLength(2));
    expect(calls.filter((c) => c.method === "PATCH").pop()!.body).toEqual({ archived: false });
  });

  it("moves to the trash through the menu, with Undo", async () => {
    server();
    renderKeep();
    await screen.findByText("Revision plan", { selector: ".kp-card-title" });
    fireEvent.click(within(card("Groceries")).getByRole("button", { name: "More" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Delete note" }));
    expect(await screen.findByText("Note moved to trash")).toBeInTheDocument();
    expect(titles()).not.toContain("Groceries");
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/notes/batch")).toBe(true));
    expect(calls.find((c) => c.url === "/v1/notes/batch")!.body).toEqual({ ids: ["n_list"], trash: true });
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(titles()).toContain("Groceries"));
  });

  it("changes the colour from the palette", async () => {
    server();
    renderKeep();
    await screen.findByText("Revision plan", { selector: ".kp-card-title" });
    fireEvent.click(within(card("Active recall")).getByRole("button", { name: "Background options" }));
    fireEvent.click(await screen.findByRole("radio", { name: "Pink" }));
    expect(card("Active recall")).toHaveAttribute("data-color", "pink");
    await waitFor(() => expect(calls.some((c) => c.method === "PATCH")).toBe(true));
    expect(calls.find((c) => c.method === "PATCH")!.body).toEqual({ color: "pink" });
  });

  it("ticks a list item right on the card", async () => {
    server();
    renderKeep();
    await screen.findByText("Revision plan", { selector: ".kp-card-title" });
    fireEvent.click(within(card("Groceries")).getByRole("checkbox", { name: "Oats" }));
    await waitFor(() => expect(within(card("Groceries")).queryByText("Oats")).toBeNull());
    expect(within(card("Groceries")).getByText("+ 2 checked items")).toBeInTheDocument();
  });
});

describe("Keep: selecting", () => {
  it("replaces the top bar with the selection bar, and Escape clears it", async () => {
    server();
    renderKeep();
    await screen.findByText("Revision plan", { selector: ".kp-card-title" });
    fireEvent.click(within(card("Active recall")).getByRole("button", { name: "Select note" }));
    fireEvent.click(within(card("Groceries")).getByRole("button", { name: "Select note" }));
    const bar = screen.getByRole("toolbar", { name: "2 selected" });
    expect(bar).toBeInTheDocument();
    expect(screen.queryByRole("searchbox")).toBeNull();
    for (const name of ["Pin note", "Remind me", "Background options", "Archive", "More", "Clear selection"]) {
      expect(within(bar).getByRole("button", { name })).toBeInTheDocument();
    }
    fireEvent.click(within(bar).getByRole("button", { name: "Archive" }));
    expect(await screen.findByText("2 notes archived")).toBeInTheDocument();
    expect(screen.getByRole("searchbox", { name: "Search" })).toBeInTheDocument();

    fireEvent.click(within(card("Revision plan")).getByRole("button", { name: "Select note" }));
    expect(screen.getByRole("toolbar", { name: "1 selected" })).toBeInTheDocument();
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("toolbar", { name: "1 selected" })).toBeNull());
  });
});

describe("Keep: the trash", () => {
  it("explains the 30 days, offers Restore and Delete forever, and empties after a confirmation", async () => {
    server();
    renderKeep("/keep/trash");
    expect(await screen.findByText("Notes in Trash are deleted after 30 days.")).toBeInTheDocument();
    const c = card("Throwaway");
    expect(within(c).getByRole("button", { name: "Restore" })).toBeInTheDocument();
    expect(within(c).getByRole("button", { name: "Delete forever" })).toBeInTheDocument();
    expect(within(c).queryByRole("button", { name: "Archive" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Empty trash" }));
    const dialog = screen.getByRole("alertdialog", { name: "Empty Trash?" });
    expect(dialog).toHaveTextContent("All notes in Trash will be permanently deleted.");
    fireEvent.click(within(dialog).getByRole("button", { name: "Empty trash" }));
    await waitFor(() => expect(calls.some((x) => x.method === "DELETE" && x.url === "/v1/notes/trash")).toBe(true));
    await waitFor(() => expect(titles()).toEqual([]));
  });

  it("restores a note from its card", async () => {
    server();
    renderKeep("/keep/trash");
    await screen.findByText("Throwaway", { selector: ".kp-card-title" });
    fireEvent.click(within(card("Throwaway")).getByRole("button", { name: "Restore" }));
    expect(await screen.findByText("Note restored")).toBeInTheDocument();
    await waitFor(() => expect(calls.find((c) => c.url === "/v1/notes/batch")?.body).toEqual({ ids: ["n_trash"], restore: true }));
  });
});

describe("Keep: the note dialog", () => {
  it("opens from /keep/note/<id>, edits, and saves on Close", async () => {
    server();
    renderKeep("/keep/note/n_text");
    const dialog = await screen.findByRole("dialog", { name: "Active recall" });
    const title = within(dialog).getByRole("textbox", { name: "Title" }) as HTMLInputElement;
    expect(title.value).toBe("Active recall");
    expect(within(dialog).getByText(/^Edited /)).toBeInTheDocument();
    fireEvent.input(title, { target: { value: "Active recall, v2" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(calls.some((c) => c.method === "PATCH" && c.url === "/v1/notes/n_text")).toBe(true));
    expect(calls.find((c) => c.method === "PATCH")!.body).toEqual({ title: "Active recall, v2" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /Active recall/ })).toBeNull());
  });

  it("closes on Escape", async () => {
    server();
    renderKeep("/keep/note/n_text");
    await screen.findByRole("dialog", { name: "Active recall" });
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Active recall" })).toBeNull());
  });

  it("edits a checklist: ticking moves an item under 'Checked items'", async () => {
    server();
    renderKeep("/keep/note/n_list");
    const dialog = await screen.findByRole("dialog", { name: "Groceries" });
    expect(within(dialog).getByText("Checked items")).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("checkbox", { name: "Oats" }));
    await waitFor(() => expect(within(dialog).getByRole("checkbox", { name: "Oats" })).toHaveAttribute("aria-checked", "true"));
    fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(calls.some((c) => c.method === "PATCH")).toBe(true));
    const items = (calls.find((c) => c.method === "PATCH")!.body as { items: { text: string; checked: boolean }[] }).items;
    expect(items.map((i) => [i.text, i.checked])).toEqual([["Oats", true], ["Lemons", true], ["Spinach", false]]);
  });

  it("opens a note from its slug in the cache, and moves the URL to its id", async () => {
    server();
    const { history } = renderKeepWithHistory("/keep/note/n-text-slug");
    const dialog = await screen.findByRole("dialog", { name: "Active recall" });
    expect(dialog).toBeInTheDocument();
    await waitFor(() => expect(history.get()).toMatch(/\/keep\/note\/n_text$/));
    expect(calls.some((c) => c.url.startsWith("/v1/notes/n-text-slug"))).toBe(false);
  });

  it("asks the server for a slug it has not synced (live notes only), then shows it", async () => {
    server();
    const extra = note({ id: "n_far", slug: "far-away", title: "Far away note", content: "from Lucina", rev: 50 });
    stubGetBySlug(extra);
    renderKeep("/keep/note/far-away");
    expect(await screen.findByRole("dialog", { name: "Far away note" })).toBeInTheDocument();
    expect(calls.some((c) => c.method === "GET" && c.url === "/v1/notes/far-away")).toBe(true);
  });

  it("says when a note does not exist", async () => {
    server();
    renderKeep("/keep/note/n_missing");
    expect(await screen.findByRole("alertdialog", { name: "This note doesn't exist any more." })).toBeInTheDocument();
  });

  it("opens a trashed note read-only", async () => {
    server();
    renderKeep("/keep/trash?note=n_trash");
    const dialog = await screen.findByRole("dialog", { name: "Throwaway" });
    expect(within(dialog).getByText(/Can't edit in Trash/)).toBeInTheDocument();
    expect(within(dialog).getByRole("textbox", { name: "Title" })).toHaveAttribute("readonly");
    expect(within(dialog).getByRole("button", { name: "Restore" })).toBeInTheDocument();
  });
});

describe("Keep: layout controls", () => {
  it("switches between grid and list view and remembers it", async () => {
    server();
    const { container } = renderKeep();
    await screen.findByText("Revision plan", { selector: ".kp-card-title" });
    const kp = container.querySelector(".kp") as HTMLElement;
    expect(kp.dataset.mode).toBe("grid");
    fireEvent.click(screen.getByRole("button", { name: "List view" }));
    expect(kp.dataset.mode).toBe("list");
    expect(screen.getByRole("button", { name: "Grid view" })).toBeInTheDocument();
    expect(localStorage.getItem("secretary.keep.list")).toBe("1");
    fireEvent.click(screen.getByRole("button", { name: "Grid view" }));
  });

  it("collapses the nav to icons", async () => {
    server();
    const { container } = renderKeep();
    await screen.findByText("Revision plan", { selector: ".kp-card-title" });
    const kp = container.querySelector(".kp") as HTMLElement;
    fireEvent.click(screen.getByRole("button", { name: "Main menu" }));
    expect(kp.dataset.nav).toBe("collapsed");
    fireEvent.click(screen.getByRole("button", { name: "Main menu" }));
    expect(kp.dataset.nav).toBe("open");
  });
});
