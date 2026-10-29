import { Route } from "@solidjs/router";
import { cleanup, fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setShowAdult } from "../lib/adult";
import { deferred, stubFetch, type Call } from "../test/fetch";
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

  it("labels the Content filter All ages, R-18 and All, the last two only while adult plugins are shown", async () => {
    serve();
    renderPixiv();
    await screen.findByRole("button", { name: "Open Work 11 by Ann" });
    const options = () => within(screen.getByLabelText("Content")).getAllByRole("option").map((o) => o.textContent);
    expect(options()).toEqual(["All ages", "R-18", "All"]);
    fireEvent.click(screen.getByRole("tab", { name: "Search" }));
    expect(options()).toEqual(["All ages", "R-18", "All"]);
    fireEvent.click(screen.getByRole("tab", { name: "Bookmarks" }));
    expect(options()).toEqual(["All ages", "R-18", "All"]);

    setShowAdult(false);
    await waitFor(() => expect(options()).toEqual(["All ages"]));
    fireEvent.click(screen.getByRole("tab", { name: "Ranking" }));
    expect(screen.queryByRole("button", { name: /R-18/ })).toBeNull();
    expect(screen.getByRole("button", { name: "Daily" })).toBeInTheDocument();
  });

  it("offers the ranking modes the content filter allows", async () => {
    const calls = serve();
    renderPixiv();
    await screen.findByRole("button", { name: "Open Work 11 by Ann" });
    expect(screen.queryByRole("button", { name: /R-18/ })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Weekly" }));
    fireEvent.change(screen.getByLabelText("Content"), { target: { value: "r18" } });
    const chips = () => within(screen.getByRole("group", { name: "Ranking mode" })).getAllByRole("button").map((b) => b.textContent);
    expect(chips()).toEqual(["Daily R-18", "Weekly R-18", "Male R-18", "Female R-18"]);
    // the weekly mode is gone, so the first R-18 one takes over
    expect(screen.getByRole("button", { name: "Daily R-18" })).toHaveAttribute("aria-pressed", "true");
    await waitFor(() => expect(calls.at(-1)!.url).toBe("/v1/pixiv/ranking?mode=day_r18&page=1&adult=1"));
    fireEvent.change(screen.getByLabelText("Content"), { target: { value: "all" } });
    expect(chips()).toHaveLength(11);
    expect(chips().slice(0, 2)).toEqual(["Daily", "Weekly"]);
    expect(screen.getByRole("button", { name: "Daily R-18" })).toHaveAttribute("aria-pressed", "true");
    fireEvent.change(screen.getByLabelText("Content"), { target: { value: "safe" } });
    expect(screen.getByRole("button", { name: "Daily" })).toHaveAttribute("aria-pressed", "true");
    await waitFor(() => expect(calls.at(-1)!.url).toBe("/v1/pixiv/ranking?mode=day&page=1&adult=1"));
  });

  it("marks R-18 works in the ranking and in a search", async () => {
    serve((c) => (c.url.startsWith("/v1/pixiv/search") ? ok({ items: [item("21"), item("22", { r18: true })], page: 1, hasMore: false }) : undefined));
    renderPixiv();
    const r18 = await screen.findByRole("button", { name: "Open Work 12 by Ann" });
    expect(r18.querySelector(".px-badge--r18")).toHaveTextContent("R-18");
    expect(screen.getByRole("button", { name: "Open Work 11 by Ann" }).querySelector(".px-badge--r18")).toBeNull();
    fireEvent.click(screen.getByRole("tab", { name: "Search" }));
    fireEvent.input(screen.getByRole("searchbox"), { target: { value: "sky" } });
    fireEvent.submit(screen.getByRole("search"));
    const hit = await screen.findByRole("button", { name: "Open Work 22 by Ann" });
    expect(hit.querySelector(".px-badge--r18")).toHaveTextContent("R-18");
    expect(screen.getByRole("button", { name: "Open Work 21 by Ann" }).querySelector(".px-badge--r18")).toBeNull();
  });

  it("falls back to the daily ranking when the switch goes off on an R-18 mode, and stops sending adult=1", async () => {
    const calls = serve();
    renderPixiv();
    await screen.findByRole("button", { name: "Open Work 11 by Ann" });
    fireEvent.change(screen.getByLabelText("Content"), { target: { value: "all" } });
    fireEvent.click(screen.getByRole("button", { name: "Daily R-18" }));
    await waitFor(() => expect(calls.some((c) => c.url.startsWith("/v1/pixiv/ranking?mode=day_r18"))).toBe(true));
    setShowAdult(false);
    await waitFor(() => expect(calls.at(-1)!.url).toBe("/v1/pixiv/ranking?mode=day&page=1"));
    expect(screen.getByRole("button", { name: "Daily" })).toHaveAttribute("aria-pressed", "true");
    expect(calls.filter((c) => c.url.includes("day_r18")).every((c) => c.url.includes("adult=1"))).toBe(true);
    expect(screen.getByLabelText("Content")).toHaveValue("safe");
  });

  it("an R-18 search choice goes back to All ages when the switch goes off", async () => {
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

  it("searches by popularity, says how, and shows the bookmark tier", async () => {
    const calls = serve((c) =>
      c.url.startsWith("/v1/pixiv/search")
        ? ok({ items: [item("21", { popularTier: 10000 }), item("22", { popularTier: 1000 }), item("23")], page: 1, hasMore: false, sort: { requested: "popular", used: c.url.includes("order=popular") ? "users_tags" : "date_d" } })
        : undefined,
    );
    renderPixiv();
    fireEvent.click(await screen.findByRole("tab", { name: "Search" }));
    const order = screen.getByLabelText("Order");
    expect(within(order).getAllByRole("option").map((o) => o.textContent)).toEqual(["Newest first", "Oldest first", "Popular"]);
    fireEvent.input(screen.getByRole("searchbox"), { target: { value: "sky" } });
    fireEvent.submit(screen.getByRole("search"));
    await screen.findByRole("button", { name: "Open Work 21 by Ann" });
    expect(screen.queryByText(/Popular by bookmark tags/)).toBeNull(); // a date order says nothing
    fireEvent.change(order, { target: { value: "popular" } });
    await waitFor(() => expect(calls.at(-1)!.url).toBe("/v1/pixiv/search?q=sky&order=popular&type=all&mode=safe&page=1&adult=1"));
    expect(await screen.findByText("Popular by bookmark tags (users入り), then the newest. Pixiv's own popular sort needs Premium.")).toBeInTheDocument();
    const top = screen.getByRole("button", { name: "Open Work 21 by Ann" });
    expect(within(top).getByRole("img", { name: "Bookmarked by 10k+ people" })).toHaveTextContent("♥ 10k+");
    expect(within(screen.getByRole("button", { name: "Open Work 22 by Ann" })).getByRole("img", { name: "Bookmarked by 1000+ people" })).toHaveTextContent("♥ 1000+");
    expect(screen.getByRole("button", { name: "Open Work 23 by Ann" }).querySelector(".px-badge--tier")).toBeNull();
  });

  it("says when Pixiv's own popular sort or the newest works stood in", async () => {
    let used = "pixiv_popular";
    serve((c) => (c.url.startsWith("/v1/pixiv/search") ? ok({ items: [item("21")], page: 1, hasMore: false, sort: { requested: "popular", used } }) : undefined));
    renderPixiv();
    fireEvent.click(await screen.findByRole("tab", { name: "Search" }));
    fireEvent.change(screen.getByLabelText("Order"), { target: { value: "popular" } });
    fireEvent.input(screen.getByRole("searchbox"), { target: { value: "sky" } });
    fireEvent.submit(screen.getByRole("search"));
    expect(await screen.findByText("Sorted by Pixiv's popularity (Premium).")).toBeInTheDocument();
    used = "date_d_fallback";
    fireEvent.change(screen.getByLabelText("Type"), { target: { value: "manga" } });
    expect(await screen.findByText("No works here have popularity tags yet, so these are the newest.")).toBeInTheDocument();
    expect(screen.queryByText("Sorted by Pixiv's popularity (Premium).")).toBeNull();
  });

  it("shows the bookmarks", async () => {
    const calls = serve();
    renderPixiv();
    fireEvent.click(await screen.findByRole("tab", { name: "Bookmarks" }));
    const card = await screen.findByRole("button", { name: "Open Work 31 by Ann" });
    expect(within(card).getByRole("img", { name: "Bookmarked" })).toBeInTheDocument();
    expect(calls.at(-1)!.url).toBe("/v1/pixiv/bookmarks?content=safe&adult=1");
  });

  it("filters the bookmarks by content and reloads them when it changes", async () => {
    const calls = serve();
    renderPixiv();
    fireEvent.click(await screen.findByRole("tab", { name: "Bookmarks" }));
    await screen.findByRole("button", { name: "Open Work 31 by Ann" });
    fireEvent.change(screen.getByLabelText("Content"), { target: { value: "r18" } });
    await waitFor(() => expect(calls.at(-1)!.url).toBe("/v1/pixiv/bookmarks?content=r18&adult=1"));
    fireEvent.change(screen.getByLabelText("Content"), { target: { value: "all" } });
    await waitFor(() => expect(calls.at(-1)!.url).toBe("/v1/pixiv/bookmarks?content=all&adult=1"));
    // the filter is shared: the search uses it too
    fireEvent.click(screen.getByRole("tab", { name: "Search" }));
    expect(screen.getByLabelText("Content")).toHaveValue("all");
    fireEvent.input(screen.getByRole("searchbox"), { target: { value: "cat" } });
    fireEvent.submit(screen.getByRole("search"));
    await waitFor(() => expect(calls.at(-1)!.url).toBe("/v1/pixiv/search?q=cat&order=date_d&type=all&mode=all&page=1&adult=1"));
  });

  describe("notices", () => {
    const NEEDS = "R-18 needs your Pixiv account. Connect it in Settings → Plugins → Pixiv.";
    const DISABLED = "Your Pixiv account hides R-18 works. On pixiv.net open Settings → Viewing restrictions and set Explicit content (R-18) to Show.";
    const ALL_AGES = "Showing all-ages works only: the plugin isn't signed in to Pixiv.";

    it("an empty R-18 ranking says the plugin needs Pixiv's session instead of Nothing here", async () => {
      serve((c) => (c.url.includes("mode=day_r18") ? ok({ items: [], page: 1, hasMore: false, notice: "r18_needs_session" }) : undefined));
      renderPixiv();
      await screen.findByRole("button", { name: "Open Work 11 by Ann" });
      fireEvent.change(screen.getByLabelText("Content"), { target: { value: "r18" } });
      expect(await screen.findByText(NEEDS)).toHaveAttribute("role", "status");
      expect(screen.getByText(NEEDS)).toHaveClass("px-notice");
      expect(screen.queryByText("Nothing here.")).toBeNull();
      fireEvent.change(screen.getByLabelText("Content"), { target: { value: "safe" } });
      await screen.findByRole("button", { name: "Open Work 11 by Ann" });
      expect(screen.queryByText(NEEDS)).toBeNull();
    });

    it("an empty search says the account hides R-18 instead of Nothing here", async () => {
      serve((c) => (c.url.startsWith("/v1/pixiv/search") ? ok({ items: [], page: 1, hasMore: false, notice: "r18_disabled_in_account" }) : undefined));
      renderPixiv();
      fireEvent.click(await screen.findByRole("tab", { name: "Search" }));
      fireEvent.change(screen.getByLabelText("Content"), { target: { value: "r18" } });
      fireEvent.input(screen.getByRole("searchbox"), { target: { value: "cat" } });
      fireEvent.submit(screen.getByRole("search"));
      expect(await screen.findByText(DISABLED)).toBeInTheDocument();
      expect(screen.queryByText("Nothing here.")).toBeNull();
    });

    it("a search for All while signed out says only all-ages works are shown, above them", async () => {
      serve((c) => (c.url.startsWith("/v1/pixiv/search") ? ok({ items: [item("21")], page: 1, hasMore: false, notice: "all_ages_only" }) : undefined));
      renderPixiv();
      fireEvent.click(await screen.findByRole("tab", { name: "Search" }));
      fireEvent.change(screen.getByLabelText("Content"), { target: { value: "all" } });
      fireEvent.input(screen.getByRole("searchbox"), { target: { value: "cat" } });
      fireEvent.submit(screen.getByRole("search"));
      expect(await screen.findByText(ALL_AGES)).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Open Work 21 by Ann" })).toBeInTheDocument();
    });

    it("the bookmarks show a notice in place of the empty note, and ignore unknown notices", async () => {
      let notice = "r18_needs_session";
      serve((c) => (c.url.startsWith("/v1/pixiv/bookmarks") && c.method === "GET" ? ok({ items: [], notice }) : undefined));
      renderPixiv();
      fireEvent.click(await screen.findByRole("tab", { name: "Bookmarks" }));
      expect(await screen.findByText(NEEDS)).toBeInTheDocument();
      expect(screen.queryByText("No bookmarks yet. Open a work and tap the bookmark.")).toBeNull();
      notice = "something_new";
      fireEvent.change(screen.getByLabelText("Content"), { target: { value: "all" } });
      expect(await screen.findByText("No bookmarks yet. Open a work and tap the bookmark.")).toBeInTheDocument();
      expect(document.querySelector(".px-notice")).toBeNull();
    });
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

    /** jsdom has no layout: give the stage a size and the page its pixels, then let it load. */
    function size(dialog: HTMLElement, stageW: number, stageH: number, imgW: number, imgH: number) {
      const stage = dialog.querySelector(".px-stage") as HTMLElement;
      const img = within(dialog).getByRole("img") as HTMLImageElement;
      Object.defineProperty(stage, "clientWidth", { configurable: true, value: stageW });
      Object.defineProperty(stage, "clientHeight", { configurable: true, value: stageH });
      Object.defineProperty(img, "naturalWidth", { configurable: true, value: imgW });
      Object.defineProperty(img, "naturalHeight", { configurable: true, value: imgH });
      fireEvent.load(img);
      return { stage, img };
    }
    const scaleOf = (img: HTMLElement) => Number(/scale\(([\d.e-]+)\)/.exec(img.style.transform)?.[1]);
    const level = (dialog: HTMLElement) => within(dialog).getByRole("button", { name: "Fit to screen" });

    it("opens a tall page in Fit (the whole image), zooms from Fit to 400 % and back, and Escape returns to Fit before it closes", async () => {
      const { dialog } = await openWork();
      // The owner's case: a 1200x1700 page in a 1400x760 viewer used to open at 100 %, cropped top and bottom.
      const { img } = size(dialog, 1400, 760, 1200, 1700);
      const fit = 760 / 1700;
      expect(scaleOf(img)).toBeCloseTo(fit, 6);
      expect(img.style.width).toBe("1200px");
      expect(level(dialog)).toHaveTextContent("Fit");
      expect(level(dialog)).toBeDisabled();
      expect(within(dialog).getByRole("button", { name: "Zoom out" })).toBeDisabled();
      fireEvent.click(within(dialog).getByRole("button", { name: "Zoom in" }));
      expect(scaleOf(img)).toBeCloseTo(fit * 1.25, 6);
      expect(level(dialog)).toHaveTextContent("56%");
      fireEvent.keyDown(window, { key: "+" });
      expect(level(dialog)).toHaveTextContent("70%");
      // while zoomed (taller than the stage) the arrows don't turn the page
      fireEvent.keyDown(window, { key: "ArrowRight" });
      expect(within(dialog).getByText("1 / 3")).toBeInTheDocument();
      fireEvent.keyDown(window, { key: "Escape" });
      expect(scaleOf(img)).toBeCloseTo(fit, 6);
      expect(screen.getByRole("dialog")).toBeInTheDocument();
      // double click: Fit -> 100 % -> Fit
      fireEvent.doubleClick(dialog.querySelector(".px-stage") as HTMLElement);
      expect(level(dialog)).toHaveTextContent("100%");
      fireEvent.doubleClick(dialog.querySelector(".px-stage") as HTMLElement);
      expect(level(dialog)).toHaveTextContent("Fit");
      // up to 400 %, then "−" all the way back down to Fit
      for (let i = 0; i < 20; i++) fireEvent.click(within(dialog).getByRole("button", { name: "Zoom in" }));
      expect(level(dialog)).toHaveTextContent("400%");
      expect(within(dialog).getByRole("button", { name: "Zoom in" })).toBeDisabled();
      for (let i = 0; i < 20; i++) fireEvent.click(within(dialog).getByRole("button", { name: "Zoom out" }));
      expect(level(dialog)).toHaveTextContent("Fit");
      expect(scaleOf(img)).toBeCloseTo(fit, 6);
      fireEvent.click(within(dialog).getByRole("button", { name: "Zoom in" }));
      fireEvent.click(level(dialog));
      expect(level(dialog)).toHaveTextContent("Fit");
    });

    it("fits wide and square pages, and never enlarges a small one", async () => {
      const { dialog } = await openWork();
      const { img } = size(dialog, 1400, 760, 2400, 1200);
      expect(scaleOf(img)).toBeCloseTo(1400 / 2400, 6);
      size(dialog, 390, 640, 1200, 1200);
      expect(scaleOf(img)).toBeCloseTo(390 / 1200, 6);
      size(dialog, 1400, 760, 400, 300);
      expect(scaleOf(img)).toBe(1);
      expect(level(dialog)).toHaveTextContent("Fit");
    });

    it("re-fits when the window changes size", async () => {
      const { dialog } = await openWork();
      const { stage, img } = size(dialog, 1400, 760, 1200, 1700);
      Object.defineProperty(stage, "clientWidth", { configurable: true, value: 700 });
      Object.defineProperty(stage, "clientHeight", { configurable: true, value: 380 });
      fireEvent(window, new Event("resize"));
      if (typeof ResizeObserver !== "undefined") fireEvent.load(img); // a stubbed observer never calls back
      expect(scaleOf(img)).toBeCloseTo(380 / 1700, 6);
    });

    it("zooms with ⌘/ctrl + scroll, pans with a plain scroll only when zoomed", async () => {
      const { dialog } = await openWork();
      const { stage, img } = size(dialog, 1400, 760, 1200, 1700);
      const fit = 760 / 1700;
      fireEvent.wheel(stage, { deltaY: 100 });
      expect(scaleOf(img)).toBeCloseTo(fit, 6); // a plain scroll at Fit does nothing
      fireEvent.wheel(stage, { deltaY: -100, ctrlKey: true });
      expect(scaleOf(img)).toBeGreaterThan(fit);
      fireEvent.wheel(stage, { deltaY: -100, metaKey: true });
      const zoomed = scaleOf(img);
      const before = img.style.transform;
      fireEvent.wheel(stage, { deltaY: 80 });
      expect(scaleOf(img)).toBeCloseTo(zoomed, 6);
      expect(img.style.transform).not.toBe(before); // panned
    });

    it("offers Fit width for a tall page and starts it at the top", async () => {
      const { dialog } = await openWork();
      const { img } = size(dialog, 1400, 760, 800, 6000);
      const button = within(dialog).getByRole("button", { name: "Fit width" });
      expect(button).toHaveAttribute("aria-pressed", "false");
      fireEvent.click(button);
      expect(button).toHaveAttribute("aria-pressed", "true");
      expect(scaleOf(img)).toBe(1);
      expect(img.style.transform).toContain(`translate(0px, ${(6000 - 760) / 2}px)`);
      fireEvent.click(button);
      expect(scaleOf(img)).toBeCloseTo(760 / 6000, 6);
    });

    it("pinches to zoom", async () => {
      const { dialog } = await openWork();
      const { stage, img } = size(dialog, 1400, 760, 1200, 1700);
      const pointer = (type: string, id: number, x: number) => fireEvent(stage, Object.assign(new Event(type, { bubbles: true }), { pointerId: id, clientX: x, clientY: 100 }));
      pointer("pointerdown", 1, 100);
      pointer("pointerdown", 2, 200);
      pointer("pointermove", 2, 300);
      expect(scaleOf(img)).toBeCloseTo((760 / 1700) * 2, 6);
      pointer("pointerup", 2, 300);
      pointer("pointerup", 1, 100);
      expect(scaleOf(img)).toBeCloseTo((760 / 1700) * 2, 6);
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

describe("Pixiv: cached lists", () => {
  beforeEach(() => setShowAdult(true));
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it("shows a revisited ranking before the server answers, then merges page one in the server's order", async () => {
    serve();
    renderPixiv();
    await screen.findByRole("button", { name: "Open Work 12 by Ann" });
    cleanup();

    const held = deferred<ReturnType<typeof ok>>();
    serve((c) => (c.url.startsWith("/v1/pixiv/ranking") ? (held.promise as never) : undefined));
    renderPixiv();
    expect(screen.getByRole("button", { name: "Open Work 11 by Ann" })).toBeInTheDocument();
    expect(screen.queryByText("Loading…")).toBeNull();
    expect(await screen.findByText("Updating…")).toBeInTheDocument();

    held.resolve(ok({ items: [item("10"), item("11", { pages: 3 })], page: 1, hasMore: true, total: null, numPages: null }));
    await waitFor(() => expect(document.querySelector(".px-card")?.getAttribute("data-work")).toBe("10"));
    // Work 12 was on the cached page and is kept after the fresh ones; "more" goes on from the copy.
    expect([...document.querySelectorAll(".px-card")].map((e) => e.getAttribute("data-work"))).toEqual(["10", "11", "12"]);
  });
});

describe("Pixiv saved defaults", () => {
  it.each([true, false])("loads defaults with adult switch %s", async adult => {
    setShowAdult(adult);
    const calls = serve(c => c.url === "/v1/pixiv/settings" ? ok({ defaultContent: "r18", defaultSort: "popular" }) : undefined);
    renderPixiv();
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Content" })).toHaveValue(adult ? "r18" : "safe"));
    fireEvent.click(screen.getByRole("tab", { name: "Search" }));
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Order" })).toHaveValue("popular"));
    fireEvent.input(screen.getByRole("searchbox"), { target: { value: "sky" } });
    fireEvent.submit(screen.getByRole("search"));
    await waitFor(() => expect(calls.some(c => c.url.includes("/search?q=sky&order=popular&type=all&mode=" + (adult ? "r18" : "safe")))).toBe(true));
    setShowAdult(true);
  });
  it("preserves the visitor's choices when defaults arrive late", async () => {
    let resolve!: (v: { defaultContent: "r18"; defaultSort: "popular" }) => void;
    const settings = vi.spyOn((await import("../api/pixiv")).pixivApi, "settings").mockImplementation(() => new Promise(r => { resolve = r; }));
    serve(); renderPixiv();
    await screen.findByRole("button", { name: "Open Work 11 by Ann" });
    fireEvent.click(screen.getByRole("tab", { name: "Search" }));
    fireEvent.change(screen.getByRole("combobox", { name: "Content" }), { target: { value: "all" } });
    fireEvent.change(screen.getByRole("combobox", { name: "Order" }), { target: { value: "date" } });
    resolve({ defaultContent: "r18", defaultSort: "popular" });
    await Promise.resolve();
    expect(screen.getByRole("combobox", { name: "Content" })).toHaveValue("all");
    expect(screen.getByRole("combobox", { name: "Order" })).toHaveValue("date");
    settings.mockRestore();
  });
  it("links the session notice to settings and labels pixiv's popular preview", async () => {
    serve(c => c.url.includes("/search?") ? ok({ items: [], notice: "r18_needs_session", sort: { requested: "popular", used: "pixiv_popular_preview" } }) : undefined);
    renderPixiv(); fireEvent.click(await screen.findByRole("tab", { name: "Search" }));
    fireEvent.input(screen.getByRole("searchbox"), { target: { value: "sky" } }); fireEvent.submit(screen.getByRole("search"));
    const notice = await screen.findByText("R-18 needs your Pixiv account. Connect it in Settings → Plugins → Pixiv.");
    expect(within(notice).getByRole("link")).toHaveAttribute("href", "/account?section=plugins");
    expect(await screen.findByText("Popular (pixiv's preview)")).toBeInTheDocument();
  });
});
