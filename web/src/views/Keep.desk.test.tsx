import { Route } from "@solidjs/router";
import { screen, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { KeepNote } from "../api/keep";
import { keep } from "../state/keep";
import { clearSelection, setSearch, setSearchOpen } from "../state/keepUi";
import { emptySearch } from "../lib/keepFilter";
import { stubFetch } from "../test/fetch";
import { note } from "../test/keepFixtures";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import KeepView from "./Keep";

function server(notes: KeepNote[]) {
  let synced = false;
  stubFetch((c) => {
    const url = new URL(c.url, "http://x");
    if (c.method === "GET" && url.pathname === "/v1/notes/changes") {
      const body = { notes: synced ? [] : notes, removed: [], cursor: "6", hasMore: false };
      synced = true;
      return { body };
    }
    if (c.method === "GET" && url.pathname === "/v1/notes/labels") return { body: { labels: [] } };
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
}

describe("Keep desktop layout", () => {
  beforeEach(async () => {
    await keep.reset();
    setSearch(emptySearch());
    setSearchOpen(false);
    clearSelection();
    stubWide();
  });
  afterEach(() => vi.unstubAllGlobals());

  it("uses the page header and section nav instead of the Keep top bar", async () => {
    server([note({ id: "n1", title: "Desk note", content: "Hello", position: 100, rev: 1 })]);
    renderAt("/keep", () => <Route path="/keep/*rest" component={KeepView} />);
    expect(await screen.findByRole("heading", { level: 1, name: "Keep" })).toBeInTheDocument();
    expect(document.querySelector(".kp-top")).toBeNull();
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(0);
    const nav = screen.getByRole("navigation", { name: "Main menu" });
    const notesLink = within(nav).getByRole("link", { name: "Notes" });
    expect(notesLink).toHaveAttribute("aria-current", "page");
    expect(await screen.findByText("Desk note")).toBeInTheDocument();
  });

  it("shows a friendly empty state on archive", async () => {
    server([]);
    renderAt("/keep/archive", () => <Route path="/keep/*rest" component={KeepView} />);
    expect(await screen.findByRole("heading", { level: 1, name: "Keep" })).toBeInTheDocument();
    expect(await screen.findByText("Your archived notes appear here")).toBeInTheDocument();
    const nav = screen.getByRole("navigation", { name: "Main menu" });
    expect(within(nav).getByRole("link", { name: "Archive" })).toHaveAttribute("aria-current", "page");
  });
});
