import { Route } from "@solidjs/router";
import { fireEvent, screen, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setShowAdult } from "../lib/adult";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import PixivView from "./Pixiv";

const item = (id: string) => ({
  id,
  title: `Work ${id}`,
  author: "Ann",
  authorId: "7",
  tags: ["sky"],
  thumbUrl: `/v1/pixiv/s/1/${id}/t`,
  pages: 1,
  r18: false,
  bookmarked: false,
});

function serve() {
  return stubFetch((call) => {
    const u = new URL(call.url, "http://x");
    if (u.pathname === "/v1/pixiv/ranking") {
      return { body: { ok: true, data: { items: [item("11")], page: 1, hasMore: false, total: null, numPages: null } } };
    }
    if (u.pathname === "/v1/pixiv/bookmarks" && call.method === "GET") {
      return { body: { ok: true, data: { items: [] } } };
    }
    return { status: 404, body: {} };
  });
}

describe("desktop Pixiv", () => {
  beforeEach(() => {
    stubWide();
    setShowAdult(true);
  });
  afterEach(() => vi.unstubAllGlobals());

  it("uses the page header, section nav and full grid card", async () => {
    serve();
    renderAt("/pixiv", () => <Route path="/pixiv/:id?" component={PixivView} />);
    expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent("Pixiv");
    const nav = screen.getByRole("navigation", { name: "Pixiv sections" });
    expect(within(nav).getByRole("button", { name: "Ranking" })).toHaveAttribute("aria-current", "page");
    expect(document.querySelector(".px-tabs")).toBeNull();
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(0);
    expect(await screen.findByRole("button", { name: "Open Work 11 by Ann" })).toBeInTheDocument();
    expect(document.querySelector(".pixiv-surface")).toBeInTheDocument();
  });

  it("switches sections from the header nav", async () => {
    serve();
    renderAt("/pixiv", () => <Route path="/pixiv/:id?" component={PixivView} />);
    await screen.findByRole("button", { name: "Open Work 11 by Ann" });
    fireEvent.click(screen.getByRole("button", { name: "Bookmarks" }));
    const nav = screen.getByRole("navigation", { name: "Pixiv sections" });
    expect(within(nav).getByRole("button", { name: "Bookmarks" })).toHaveAttribute("aria-current", "page");
    expect(await screen.findByText("No bookmarks yet. Open a work and tap the bookmark.")).toBeInTheDocument();
  });
});
