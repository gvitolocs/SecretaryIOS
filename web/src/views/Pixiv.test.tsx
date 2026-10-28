import { Route } from "@solidjs/router";
import { cleanup, fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setShowAdult } from "../lib/adult";
import { stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
import PixivView from "./Pixiv";

const item = (id: string, over: Record<string, unknown> = {}) => ({
  id, title: `Work ${id}`, author: "Ann", authorId: "7", tags: ["sky"], thumbUrl: `/v1/pixiv/s/1/${id}/t`, pages: 1, r18: false, bookmarked: false, ...over,
});

const WORK = {
  id: "11", title: "Work 11", author: "Ann", authorId: "7", created: "2026-10-01T00:00:00+00:00", kind: "manga", ugoira: false, r18: false,
  description: "A quiet street.", stats: { views: 1200, likes: 30, bookmarks: 5, comments: 0 }, pageCount: 3, coverUrl: "/v1/pixiv/s/1/11/c",
  tags: [{ tag: "風景", en: "landscape" }, { tag: "sky", en: "" }],
  pages: [0, 1, 2].map((i) => ({ index: i, thumbUrl: `/v1/pixiv/s/1/11/t${i}`, url: `/v1/pixiv/s/1/11/p${i}`, originalUrl: `/v1/pixiv/s/1/11/o${i}` })),
  bookmarked: false,
};

function ok(data: unknown) {
  return { body: { ok: true, data } };
}

function serve(extra?: (c: Call) => ReturnType<Parameters<typeof stubFetch>[0]> | undefined) {
  return stubFetch((call) => {
    const custom = extra?.(call);
    if (custom) return custom;
    const u = new URL(call.url, "http://x");
    if (u.pathname === "/v1/pixiv/ranking") {
      const page = Number(u.searchParams.get("page"));
      return ok({ items: page === 1 ? [item("11", { pages: 3 }), item("12", { r18: true })] : [item("13")], page, hasMore: page === 1, total: null, numPages: null });
    }
    if (u.pathname === "/v1/pixiv/search") return ok({ items: [item("21", { title: `Hit for ${u.searchParams.get("q")}` })], page: 1, hasMore: false, total: 1, numPages: 1 });
    if (u.pathname === "/v1/pixiv/illust/11") return ok(WORK);
    if (u.pathname === "/v1/pixiv/bookmarks" && call.method === "GET") return ok({ items: [item("31", { bookmarked: true })] });
    if (u.pathname.startsWith("/v1/pixiv/bookmarks/")) return ok({ bookmarked: call.method === "PUT" });
    if (u.pathname === "/v1/pixiv/save") return ok({ saved: [{ page: 1, itemId: "itm_1", duplicate: false }], failed: [] });
    return { status: 404, body: {} };
  });
}

function renderPixiv(path = "/pixiv") {
  renderAt(path, () => <Route path="/pixiv/:id?" component={PixivView} />);
}

describe("Pixiv", () => {
  beforeEach(() => setShowAdult(true));
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    setShowAdult(true);
  });

  it("opens on the daily ranking and shows its works", async () => {
    const calls = serve();
    renderPixiv();
    expect(await screen.findByRole("button", { name: "Open Work 11 by Ann" })).toBeInTheDocument();
    expect(calls[0]!.url).toBe("/v1/pixiv/ranking?mode=day&page=1&adult=1");
    expect(screen.getByRole("tab", { name: "Ranking" })).toHaveAttribute("aria-selected", "true");
    const card = screen.getByRole("button", { name: "Open Work 11 by Ann" });
    expect(within(card).getByText("3")).toBeInTheDocument(); // the page count
    expect(card.querySelector("img")).toHaveAttribute("src", "/v1/pixiv/s/1/11/t");
  });

  it("loads the next page on demand and does not repeat a work", async () => {
    const calls = serve();
    renderPixiv();
    await screen.findByRole("button", { name: "Open Work 11 by Ann" });
    fireEvent.click(screen.getByRole("button", { name: "Load more" }));
    await screen.findByRole("button", { name: "Open Work 13 by Ann" });
    expect(calls.at(-1)!.url).toBe("/v1/pixiv/ranking?mode=day&page=2&adult=1");
    expect(screen.queryByRole("button", { name: "Load more" })).toBeNull();
    expect(screen.getByText("That's all for now.")).toBeInTheDocument();
  });

  it("switches ranking mode", async () => {
    const calls = serve();
    renderPixiv();
    await screen.findByRole("button", { name: "Open Work 11 by Ann" });
    fireEvent.click(screen.getByRole("button", { name: "Weekly" }));
    await waitFor(() => expect(calls.some((c) => c.url.startsWith("/v1/pixiv/ranking?mode=week"))).toBe(true));
  });

  it("lists R-18 ranking modes and the R-18 search filter only while adult plugins are shown", async () => {
    serve();
    renderPixiv();
    await screen.findByRole("button", { name: "Open Work 11 by Ann" });
    expect(screen.getByRole("button", { name: "Daily R-18" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Weekly R-18" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("tab", { name: "Search" }));
    expect(within(screen.getByLabelText("Content")).getByRole("option", { name: "R-18" })).toBeInTheDocument();

    setShowAdult(false);
    await waitFor(() => expect(within(screen.getByLabelText("Content")).queryByRole("option", { name: "R-18" })).toBeNull());
    fireEvent.click(screen.getByRole("tab", { name: "Ranking" }));
    expect(screen.queryByRole("button", { name: /R-18/ })).toBeNull();
    expect(screen.getByRole("button", { name: "Daily" })).toBeInTheDocument();
  });

  it("falls back to the daily ranking when the switch goes off on an R-18 mode, and stops sending adult=1", async () => {
    const calls = serve();
    renderPixiv();
    await screen.findByRole("button", { name: "Open Work 11 by Ann" });
    fireEvent.click(screen.getByRole("button", { name: "Daily R-18" }));
    await waitFor(() => expect(calls.some((c) => c.url.startsWith("/v1/pixiv/ranking?mode=day_r18"))).toBe(true));
    setShowAdult(false);
    await waitFor(() => expect(calls.at(-1)!.url).toBe("/v1/pixiv/ranking?mode=day&page=1"));
    expect(screen.getByRole("button", { name: "Daily" })).toHaveAttribute("aria-pressed", "true");
    expect(calls.filter((c) => c.url.includes("day_r18")).every((c) => c.url.includes("adult=1"))).toBe(true);
  });

  it("an R-18 search choice goes back to Safe when the switch goes off", async () => {
    const calls = serve();
    renderPixiv();
    fireEvent.click(await screen.findByRole("tab", { name: "Search" }));
    const content = screen.getByLabelText("Content") as HTMLSelectElement;
    fireEvent.change(content, { target: { value: "r18" } });
    expect(content.value).toBe("r18");
    setShowAdult(false);
    await waitFor(() => expect((screen.getByLabelText("Content") as HTMLSelectElement).value).toBe("safe"));
    fireEvent.input(screen.getByRole("searchbox"), { target: { value: "cat" } });
    fireEvent.submit(screen.getByRole("search"));
    await screen.findByRole("button", { name: /Hit for cat/ });
    expect(calls.at(-1)!.url).toBe("/v1/pixiv/search?q=cat&order=date_d&type=all&mode=safe&page=1");
  });

  it("searches with the chosen order and type", async () => {
    const calls = serve();
    renderPixiv();
    fireEvent.click(await screen.findByRole("tab", { name: "Search" }));
    expect(screen.getByText("Type a tag or a word to search.")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Order"), { target: { value: "date" } });
    fireEvent.change(screen.getByLabelText("Type"), { target: { value: "manga" } });
    fireEvent.input(screen.getByRole("searchbox"), { target: { value: "blue sky" } });
    fireEvent.submit(screen.getByRole("search"));
    await screen.findByRole("button", { name: /Hit for blue sky/ });
    expect(calls.at(-1)!.url).toBe("/v1/pixiv/search?q=blue+sky&order=date&type=manga&mode=safe&page=1&adult=1");
  });

  it("shows the bookmarks", async () => {
    serve();
    renderPixiv();
    fireEvent.click(await screen.findByRole("tab", { name: "Bookmarks" }));
    const card = await screen.findByRole("button", { name: "Open Work 31 by Ann" });
    expect(within(card).getByRole("img", { name: "Bookmarked" })).toBeInTheDocument();
  });

  it("says when Pixiv asks to slow down, and retries", async () => {
    let first = true;
    serve((c) => {
      if (first && c.url.startsWith("/v1/pixiv/ranking")) {
        first = false;
        return { status: 429, body: { ok: false, error: "rate_limited", retryAfter: 12 }, headers: { "Retry-After": "12" } };
      }
      return undefined;
    });
    renderPixiv();
    expect(await screen.findByText("Pixiv asked us to slow down. Try again in 12 s.")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("button", { name: "Open Work 11 by Ann" })).toBeInTheDocument();
  });

  describe("the reader", () => {
    async function openWork() {
      const calls = serve();
      renderPixiv();
      fireEvent.click(await screen.findByRole("button", { name: "Open Work 11 by Ann" }));
      const dialog = await screen.findByRole("dialog", { name: "Work viewer" });
      await waitFor(() => expect(within(dialog).getByText("1 / 3")).toBeInTheDocument());
      await waitFor(() => expect(within(dialog).getByRole("img")).toHaveAttribute("src", "/v1/pixiv/s/1/11/p0"));
      return { calls, dialog };
    }

    it("shows the first page and turns pages with the buttons and the arrow keys", async () => {
      const { dialog, calls } = await openWork();
      expect(calls.some((c) => c.url === "/v1/pixiv/illust/11?adult=1")).toBe(true);
      expect(within(dialog).queryByRole("button", { name: "Previous page" })).toBeNull();
      fireEvent.click(within(dialog).getByRole("button", { name: "Next page" }));
      expect(within(dialog).getByText("2 / 3")).toBeInTheDocument();
      expect(within(dialog).getByRole("img")).toHaveAttribute("src", "/v1/pixiv/s/1/11/p1");
      fireEvent.keyDown(window, { key: "ArrowRight" });
      expect(within(dialog).getByText("3 / 3")).toBeInTheDocument();
      expect(within(dialog).queryByRole("button", { name: "Next page" })).toBeNull();
      fireEvent.keyDown(window, { key: "ArrowRight" });
      expect(within(dialog).getByText("3 / 3")).toBeInTheDocument();
      fireEvent.keyDown(window, { key: "ArrowLeft" });
      expect(within(dialog).getByText("2 / 3")).toBeInTheDocument();
    });

    it("turns pages by swiping", async () => {
      const { dialog } = await openWork();
      const stage = dialog.querySelector(".px-stage") as HTMLElement;
      const pointer = (type: string, x: number, y = 100) => fireEvent(stage, Object.assign(new Event(type, { bubbles: true }), { pointerId: 1, clientX: x, clientY: y }));
      pointer("pointerdown", 300);
      pointer("pointermove", 200);
      pointer("pointerup", 200);
      expect(within(dialog).getByText("2 / 3")).toBeInTheDocument();
      pointer("pointerdown", 100);
      pointer("pointerup", 220);
      expect(within(dialog).getByText("1 / 3")).toBeInTheDocument();
      // a short drag or a mostly vertical one does nothing
      pointer("pointerdown", 100, 100);
      pointer("pointerup", 130, 100);
      pointer("pointerdown", 100, 100);
      pointer("pointerup", 200, 300);
      expect(within(dialog).getByText("1 / 3")).toBeInTheDocument();
    });

    it("preloads the next page", async () => {
      const loaded: string[] = [];
      const Real = globalThis.Image;
      vi.stubGlobal("Image", class { set src(v: string) { loaded.push(v); } });
      try {
        await openWork();
        await waitFor(() => expect(loaded).toContain("/v1/pixiv/s/1/11/p1"));
        fireEvent.keyDown(window, { key: "ArrowRight" });
        await waitFor(() => expect(loaded).toContain("/v1/pixiv/s/1/11/p2"));
      } finally {
        vi.stubGlobal("Image", Real);
      }
    });

    it("zooms with the buttons, keys and a double click, and Escape un-zooms before it closes", async () => {
      const { dialog } = await openWork();
      const img = within(dialog).getByRole("img");
      expect(img.style.transform).toContain("scale(1)");
      fireEvent.click(within(dialog).getByRole("button", { name: "Zoom in" }));
      expect(img.style.transform).toContain("scale(1.5)");
      fireEvent.keyDown(window, { key: "+" });
      expect(img.style.transform).toContain("scale(2)");
      // while zoomed the arrows don't turn the page
      fireEvent.keyDown(window, { key: "ArrowRight" });
      expect(within(dialog).getByText("1 / 3")).toBeInTheDocument();
      fireEvent.keyDown(window, { key: "Escape" });
      expect(img.style.transform).toContain("scale(1)");
      expect(screen.getByRole("dialog")).toBeInTheDocument();
      fireEvent.doubleClick(dialog.querySelector(".px-stage") as HTMLElement);
      expect(img.style.transform).toContain("scale(2.5)");
      fireEvent.click(within(dialog).getByRole("button", { name: "Fit to screen" }));
      expect(img.style.transform).toContain("scale(1)");
    });

    it("pinches to zoom", async () => {
      const { dialog } = await openWork();
      const stage = dialog.querySelector(".px-stage") as HTMLElement;
      const img = within(dialog).getByRole("img");
      const pointer = (type: string, id: number, x: number) => fireEvent(stage, Object.assign(new Event(type, { bubbles: true }), { pointerId: id, clientX: x, clientY: 100 }));
      pointer("pointerdown", 1, 100);
      pointer("pointerdown", 2, 200);
      pointer("pointermove", 2, 300);
      expect(img.style.transform).toContain("scale(2)");
      pointer("pointerup", 2, 300);
      pointer("pointerup", 1, 100);
      expect(img.style.transform).toContain("scale(2)");
    });

    it("closes with the button and with Escape", async () => {
      const { dialog } = await openWork();
      fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
      await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
      fireEvent.click(screen.getByRole("button", { name: "Open Work 11 by Ann" }));
      await screen.findByRole("dialog");
      fireEvent.keyDown(window, { key: "Escape" });
      await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    });

    it("bookmarks and un-bookmarks, and the card follows", async () => {
      const { dialog, calls } = await openWork();
      const toggle = within(dialog).getByRole("button", { name: "Bookmark" });
      fireEvent.click(toggle);
      await waitFor(() => expect(calls.some((c) => c.method === "PUT" && c.url === "/v1/pixiv/bookmarks/11?adult=1")).toBe(true));
      expect(within(dialog).getByRole("button", { name: "Remove bookmark" })).toHaveAttribute("aria-pressed", "true");
      fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
      await waitFor(() => expect(within(screen.getByRole("button", { name: "Open Work 11 by Ann" })).getByRole("img", { name: "Bookmarked" })).toBeInTheDocument());
    });

    it("saves the whole work or only the current page to Lucina", async () => {
      const { dialog, calls } = await openWork();
      fireEvent.click(within(dialog).getByRole("button", { name: "Save to Lucina" }));
      fireEvent.click(within(dialog).getByRole("menuitem", { name: "Whole work" }));
      await waitFor(() => expect(within(dialog).getByRole("status")).toHaveTextContent("Saved 1 to the Gallery."));
      const whole = calls.filter((c) => c.url.startsWith("/v1/pixiv/save")).at(-1)!;
      expect(whole.body).toEqual({ id: "11" });
      expect(whole.headers["x-secretary-csrf"]).toBe("1");

      fireEvent.keyDown(window, { key: "ArrowRight" });
      fireEvent.click(within(dialog).getByRole("button", { name: "Save to Lucina" }));
      fireEvent.click(within(dialog).getByRole("menuitem", { name: "This page" }));
      await waitFor(() => expect(calls.filter((c) => c.url.startsWith("/v1/pixiv/save"))).toHaveLength(2));
      expect(calls.filter((c) => c.url.startsWith("/v1/pixiv/save")).at(-1)!.body).toEqual({ id: "11", pages: [1] });
    });

    it("lists the tags, and a tag opens a search for it", async () => {
      const { dialog, calls } = await openWork();
      fireEvent.click(within(dialog).getByRole("button", { name: "Details" }));
      expect(within(dialog).getByText("A quiet street.")).toBeInTheDocument();
      expect(within(dialog).getByText("1,200")).toBeInTheDocument();
      expect(within(dialog).getByText("landscape")).toBeInTheDocument();
      fireEvent.click(within(dialog).getByRole("button", { name: "Search for 風景" }));
      await screen.findByRole("button", { name: /Hit for 風景/ });
      expect(screen.queryByRole("dialog")).toBeNull();
      expect(screen.getByRole("tab", { name: "Search" })).toHaveAttribute("aria-selected", "true");
      expect(decodeURIComponent(calls.at(-1)!.url)).toContain("q=風景");
    });

    it("says so when the work is adult and the switch is off", async () => {
      setShowAdult(false);
      serve((c) => (c.url.startsWith("/v1/pixiv/illust/") ? { status: 403, body: { ok: false, error: "adult_off" } } : undefined));
      renderPixiv("/pixiv/12");
      expect(await screen.findByText("Adult content is switched off on this device.")).toBeInTheDocument();
    });
  });
});
