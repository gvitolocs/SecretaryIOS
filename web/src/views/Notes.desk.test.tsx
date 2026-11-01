import { Route } from "@solidjs/router";
import { screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import NotesView from "./Notes";

const BRIEFING = {
  id: "n_00000000000000000001",
  slug: "craftit-meeting",
  title: "CRAFT-IT4SD — meeting with Marianne & Jonas",
  content: "# Briefing",
  tags: ["work"],
  source: "chatgpt",
  pinned: true,
  createdAt: "2026-10-22T11:30:00.000Z",
  updatedAt: "2026-10-22T11:31:00.000Z",
};

function server() {
  return stubFetch((c) => {
    const url = new URL(c.url, "http://x");
    if (c.method === "GET" && url.pathname === "/v1/notes") {
      return { body: { notes: [{ ...BRIEFING, size: 10, excerpt: "Briefing" }] } };
    }
    if (c.method === "GET" && url.pathname === "/v1/notes/craftit-meeting") return { body: BRIEFING };
    return { status: 404, body: { error: { code: "note_not_found", message: "no" } } };
  });
}

describe("Notes desktop layout", () => {
  beforeEach(() => stubWide());
  afterEach(() => vi.unstubAllGlobals());

  it("uses the page header, split list and pick-note empty state", async () => {
    server();
    renderAt("/notes", () => <Route path="/notes/:key?" component={NotesView} />);
    expect(await screen.findByRole("heading", { level: 1, name: "Notes" })).toBeInTheDocument();
    expect(document.querySelector(".notes-head")).toBeNull();
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(1);
    expect(screen.getByRole("link", { name: "New note" })).toHaveClass("pg-cta");
    expect(screen.getByRole("link", { name: "Trash" })).toHaveClass("pg-icon-btn");
    const side = screen.getByRole("navigation", { name: "Notes" });
    expect(within(side).getByRole("searchbox", { name: "Search notes" })).toBeInTheDocument();
    expect(await within(side).findByRole("link", { name: /CRAFT-IT4SD/ })).toBeInTheDocument();
    expect(await screen.findByText("Pick a note from the list, or start a new one.")).toBeInTheDocument();
  });

  it("shows list rows and the selected note in the split panes", async () => {
    server();
    renderAt("/notes/craftit-meeting", () => <Route path="/notes/:key?" component={NotesView} />);
    expect(await screen.findByRole("heading", { level: 1, name: "Notes" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { level: 1, name: "CRAFT-IT4SD — meeting with Marianne & Jonas" })).toBeInTheDocument();
    await waitFor(() => expect(document.querySelector(".notes-desk-row--active")).toBeTruthy());
  });
});
