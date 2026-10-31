import { Route } from "@solidjs/router";
import { cleanup, fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { IgItem, IgPage } from "../api/instagram";
import { pageCache } from "../lib/pageCache";
import { deferred, stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import { instagramBadges, resetInstagramBadges } from "../state/instagramBadge";
import InstagramView, { errorText } from "./Instagram";

const item = (id: string, kind: IgItem["kind"] = "image", extra: Partial<IgItem> = {}): IgItem => ({
  id,
  kind,
  url: `/v1/instagram/s/9/x/${id}`,
  thumbUrl: `/v1/instagram/s/9/x/${id}-t`,
  takenAt: 1_700_000_000,
  caption: `caption ${id}`,
  ...extra,
});

const posts = (ids: string[], next: string | null = null, exhausted?: boolean): IgPage => ({ items: ids.map((i) => item(i)), nextCursor: next, ...(exhausted ? { exhausted } : {}) });

const bundle = (over: { hasStories?: boolean; isPrivate?: boolean; highlights?: boolean; highlightCount?: number | null; next?: string | null } = {}) => ({
  profile: {
    username: "natgeo",
    fullName: "National Geographic",
    biography: "Step into wonder",
    picUrl: "/v1/instagram/s/9/x/pfp",
    profileUrl: "https://www.instagram.com/natgeo/",
    mediaCount: 32000,
    followerCount: 268_000_000,
    followingCount: 180,
    isPrivate: over.isPrivate ?? false,
    hasStories: over.hasStories ?? false,
    isVerified: true,
    userId: "787132",
    externalUrl: "https://example.test/x",
  },
  posts: posts(["p1", "p2", "p3"], over.next === undefined ? "C2" : over.next),
  highlights: over.highlights ? [{ id: "highlight:1", title: "Best of", coverUrl: "/v1/instagram/s/9/x/h", mediaCount: over.highlightCount === undefined ? 2 : over.highlightCount }] : [],
});

interface Routes {
  profile?: unknown;
  profileStatus?: number;
  [k: string]: unknown;
}

function serve(r: Routes) {
  return stubFetch((call: Call) => {
    const url = call.url;
    if (url.startsWith("/v1/instagram/profile/")) return { status: r.profileStatus ?? 200, body: r.profileStatus ? { ok: false, error: "not_found" } : { ok: true, data: r.profile } };
    if (url.startsWith("/v1/instagram/posts/")) return { body: { ok: true, data: r.posts } };
    if (url.startsWith("/v1/instagram/reels/")) return { body: { ok: true, data: r.reels ?? { items: [], nextCursor: null } } };
    if (url.startsWith("/v1/instagram/stories/")) return { body: { ok: true, data: r.stories ?? { items: [], nextCursor: null } } };
    if (url.startsWith("/v1/instagram/highlights/items")) return { body: { ok: true, data: r.highlightItems } };
    if (url.startsWith("/v1/instagram/highlights/")) return { status: r.highlightsStatus as number | undefined, body: r.highlightsStatus ? { ok: false, error: "upstream_unavailable" } : { ok: true, data: { items: r.highlights ?? [] } } };
    if (url.startsWith("/v1/instagram/resolve")) return { body: { ok: true, data: r.resolve } };
    if (url === "/v1/instagram/watch" && call.method === "GET") return { body: { ok: true, data: r.watched ?? [] } };
    if (url === "/v1/instagram/watch-settings") {
      if (call.method === "PUT" && r.settingsStatus) return { status: r.settingsStatus as number, body: { ok: false, error: "bad_request" } };
      const hours = call.method === "PUT" ? (call.body as { intervalHours: number }).intervalHours : ((r.intervalHours as number | undefined) ?? 23);
      return { body: { ok: true, data: { intervalHours: hours, defaultHours: 23, jitterMinutes: 30, choices: [6, 12, 23] } } };
    }
    if (url.startsWith("/v1/instagram/watch/") && url.endsWith("/opened")) return { body: { ok: true, data: r.opened ?? { total: 0, accounts: {} } } };
    if (url.startsWith("/v1/instagram/watch/")) return { body: { ok: true, data: { username: "natgeo", autoSave: true } } };
    if (url === "/v1/instagram/badges") return { body: { ok: true, data: { total: 0, accounts: {} } } };
    if (url.startsWith("/v1/instagram/archive/")) return { body: { ok: true, data: r.archive ?? { items: [], nextCursor: null } } };
    if (url === "/v1/instagram/recent") return { body: { ok: true, data: { items: r.recent ?? [] } } };
    if (url === "/v1/instagram/save") return { body: { ok: true, data: r.save ?? { saved: [{ index: 0, itemId: "itm_1", duplicate: false }], failed: [] } } };
    return { status: 404, body: {} };
  });
}

function renderIg(path = "/instagram") {
  return renderAt(path, () => <Route path="/instagram/:username?" component={InstagramView} />);
}

// Visited pages open on their last copy while the server is asked again (owner, 2026-10-29: "the pages that I
// visit should be cached ... just check and fetch for updates").
describe("Instagram: cached pages", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    resetInstagramBadges();
  });

  it("shows a revisited profile before the server answers, then merges the new post in front", async () => {
    serve({ profile: bundle() });
    renderIg("/instagram/natgeo");
    await screen.findByRole("button", { name: /^Posts 3/ });
    cleanup();

    const held = deferred<{ body: unknown }>();
    stubFetch((call) => {
      if (call.url.startsWith("/v1/instagram/profile/")) return held.promise;
      return { body: { ok: true, data: call.url.endsWith("/badges") || call.url.endsWith("/opened") ? { total: 0, accounts: {} } : [] } };
    });
    renderIg("/instagram/natgeo");
    // The copy, on the first frame: no "Loading…", the old posts, and the quiet "Updating…" pill.
    expect(screen.getByRole("heading", { name: /National Geographic/ })).toBeInTheDocument();
    expect(screen.queryByText("Loading…")).toBeNull();
    expect(screen.getByRole("button", { name: /^Posts 1: caption p1/ })).toBeInTheDocument();
    expect(await screen.findByText("Updating…")).toHaveClass("swr-updating");

    const update = bundle();
    held.resolve({ body: { ok: true, data: { ...update, posts: posts(["p0", "p1", "p2", "p3"], null) } } });
    expect(await screen.findByRole("button", { name: /^Posts 1: caption p0/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^Posts 4: caption p3/ })).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText("Updating…")).toBeNull());
  });

  it("comes back (Back) from memory at once, on the same tab and scroll offset", async () => {
    let scrollTop = 0;
    const Scrolling = () => (
      <main
        class="content"
        ref={(el) => {
          Object.defineProperty(el, "scrollHeight", { configurable: true, get: () => 4000 });
          Object.defineProperty(el, "clientHeight", { configurable: true, get: () => 800 });
          Object.defineProperty(el, "scrollTop", { configurable: true, get: () => scrollTop, set: (v: number) => (scrollTop = v) });
        }}
      >
        <InstagramView />
      </main>
    );
    serve({ profile: bundle(), reels: posts(["r1", "r2"]) });
    renderAt("/instagram/natgeo", () => <Route path="/instagram/:username?" component={Scrolling} />);
    fireEvent.click(await screen.findByRole("tab", { name: "Reels" }));
    await screen.findByRole("button", { name: /^Reels 2/ });
    scrollTop = 640;
    document.querySelector("main.content")!.dispatchEvent(new Event("scroll"));
    cleanup();
    scrollTop = 0;

    serve({ profile: bundle(), reels: posts(["r1", "r2"]) });
    renderAt("/instagram/natgeo", () => <Route path="/instagram/:username?" component={Scrolling} />);
    expect(screen.queryByText("Loading…")).toBeNull();
    expect(screen.getByRole("tab", { name: "Reels" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("button", { name: /^Reels 2/ })).toBeInTheDocument();
    await waitFor(() => expect(scrollTop).toBe(640));
  });

  it("opens a cached highlight on its tiles at once and refreshes it", async () => {
    await pageCache.put("instagram:highlight:highlight:1", "instagram", { items: [item("h1"), item("h2")], nextCursor: null });
    const held = deferred<IgPage>();
    serve({ profile: bundle({ highlights: true }) });
    // Everything as usual, except the highlight's items, which answer when the test says.
    const usual = globalThis.fetch;
    let asked = 0;
    vi.stubGlobal("fetch", async (input: RequestInfo | URL, init?: RequestInit) => {
      if (!String(input).startsWith("/v1/instagram/highlights/items")) return usual(input, init);
      asked++;
      const page = await held.promise;
      return new Response(JSON.stringify({ ok: true, data: page }), { headers: { "Content-Type": "application/json" } });
    });
    renderIg("/instagram/natgeo");
    fireEvent.click(await screen.findByRole("tab", { name: "Highlights" }));
    fireEvent.click(screen.getByRole("button", { name: /Best of/ }));
    // The copy's two items, before the server answered (jsdom gives the grid no size, so its header counts them).
    const tiles = await screen.findByRole("dialog", { name: /Best of/ });
    expect(tiles).toHaveTextContent("2 items");
    expect(asked).toBe(1);
    held.resolve({ items: [item("h0"), item("h1"), item("h2")], nextCursor: null });
    await waitFor(() => expect(tiles).toHaveTextContent("3 items"));
  });
});

describe("Instagram", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    resetInstagramBadges();
    delete (window as { secretaryShell?: unknown }).secretaryShell;
  });

  it("lists the recent profiles and opens one", async () => {
    const calls = serve({ recent: [{ username: "nasa", fullName: "NASA", isVerified: true, picUrl: null, openedAt: 1 }], profile: { ...bundle(), profile: { ...bundle().profile, username: "nasa", fullName: "NASA" } } });
    renderIg();
    fireEvent.click(await screen.findByRole("button", { name: /NASA/ }));
    expect(await screen.findByRole("heading", { name: /NASA/ })).toBeInTheDocument();
    expect(calls.some((c) => c.url === "/v1/instagram/profile/nasa")).toBe(true);
  });

  it("says so when nothing was opened yet", async () => {
    serve({});
    renderIg();
    expect(await screen.findByText("Profiles you open show up here.")).toBeInTheDocument();
  });

  it("opens a typed username, ignoring @ and case", async () => {
    const calls = serve({ profile: bundle() });
    renderIg();
    fireEvent.input(screen.getByRole("searchbox", { name: "Open a profile or a link" }), { target: { value: " @NatGeo " } });
    fireEvent.click(screen.getByRole("button", { name: "Open" }));
    expect(await screen.findByText("@natgeo")).toBeInTheDocument();
    expect(calls.some((c) => c.url === "/v1/instagram/profile/natgeo")).toBe(true);
  });

  it("refuses text that is neither a username nor an Instagram link", async () => {
    const calls = serve({});
    renderIg();
    fireEvent.input(screen.getByRole("searchbox"), { target: { value: "not a name!" } });
    fireEvent.click(screen.getByRole("button", { name: "Open" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Enter a username");
    expect(calls.filter((c) => c.url.startsWith("/v1/instagram/profile"))).toHaveLength(0);
  });

  it("shows the profile header, counts and the five tabs", async () => {
    serve({ profile: bundle() });
    renderIg("/instagram/natgeo");
    expect(await screen.findByRole("heading", { name: /National Geographic/ })).toBeInTheDocument();
    expect(screen.getByRole("img", { name: "Verified" })).toBeInTheDocument();
    expect(screen.getByText("268M")).toBeInTheDocument();
    expect(screen.getByText("32K")).toBeInTheDocument();
    expect(screen.getByText("Step into wonder")).toBeInTheDocument();
    expect(screen.getAllByRole("tab").map((t) => t.textContent)).toEqual(["Stories", "Highlights", "Posts", "Reels", "Archive"]);
    expect(screen.getByRole("tab", { name: "Posts" })).toHaveAttribute("aria-selected", "true");
    expect(within(screen.getByRole("tabpanel")).getAllByRole("button")).toHaveLength(4); // 3 tiles + Load more
  });

  it("labels the bio, and opens the profile picture in the viewer", async () => {
    serve({ profile: bundle({ isPrivate: true }) });
    renderIg("/instagram/natgeo");
    expect((await screen.findByText("Step into wonder")).closest(".ig-bio")).toHaveTextContent(/^BioStep into wonder$/);
    const pic = screen.getByRole("button", { name: "Show the profile picture of @natgeo" });
    expect(pic.querySelector("img")).toHaveAttribute("src", "/v1/instagram/s/9/x/pfp");
    fireEvent.click(pic);
    const viewer = await screen.findByRole("dialog", { name: "Profile picture of @natgeo" });
    expect(viewer.querySelector("img[src='/v1/instagram/s/9/x/pfp']")).not.toBeNull();
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("starts on Stories when there are some and loads them once", async () => {
    const calls = serve({ profile: bundle({ hasStories: true }), stories: { items: [item("s1", "video"), item("s2")], nextCursor: null } });
    renderIg("/instagram/natgeo");
    expect(await screen.findByRole("tab", { name: "Stories", selected: true })).toBeInTheDocument();
    await waitFor(() => expect(within(screen.getByRole("tabpanel")).getAllByRole("button")).toHaveLength(2));
    fireEvent.click(screen.getByRole("tab", { name: "Posts" }));
    fireEvent.click(screen.getByRole("tab", { name: "Stories" }));
    expect(calls.filter((c) => c.url.startsWith("/v1/instagram/stories/"))).toHaveLength(1);
  });

  it("loads reels the first time the tab opens and pages them", async () => {
    const calls = serve({ profile: bundle(), reels: { items: [item("r1", "video"), item("r2", "video")], nextCursor: "R2" } });
    renderIg("/instagram/natgeo");
    fireEvent.click(await screen.findByRole("tab", { name: "Reels" }));
    await waitFor(() => expect(screen.getAllByRole("button", { name: /^Reels \d/ })).toHaveLength(2));
    expect(calls.filter((c) => c.url.startsWith("/v1/instagram/reels/"))).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Load more" }));
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/instagram/reels/natgeo?cursor=R2")).toBe(true));
  });

  it("loads more posts, skips ones it already has, and stops at exhausted", async () => {
    const calls = serve({ profile: bundle(), posts: posts(["p3", "p4"], "C3") });
    renderIg("/instagram/natgeo");
    fireEvent.click(await screen.findByRole("button", { name: "Load more" }));
    await waitFor(() => expect(screen.getAllByRole("button", { name: /^Posts \d/ })).toHaveLength(4));
    expect(calls.some((c) => c.url === "/v1/instagram/posts/natgeo?cursor=C2")).toBe(true);
  });

  it("hides Load more when the server says the list is exhausted", async () => {
    serve({ profile: { ...bundle(), posts: posts(["p1"], "C2", true) } });
    renderIg("/instagram/natgeo");
    await screen.findByRole("button", { name: /^Posts 1/ });
    expect(screen.queryByRole("button", { name: "Load more" })).toBeNull();
  });

  it("asks again once for a tray from an earlier visit, whose cover URLs may have expired", async () => {
    await pageCache.put("instagram:tray:natgeo", "instagram", [{ id: "highlight:1", title: "Best of", coverUrl: "/v1/instagram/s/1/old/h", mediaCount: 2 }]);
    const calls = serve({ profile: bundle(), highlights: bundle({ highlights: true }).highlights });
    renderIg("/instagram/natgeo");
    fireEvent.click(await screen.findByRole("tab", { name: "Highlights" }));
    await screen.findByRole("button", { name: /Best of/ });
    await waitFor(() => expect(screen.getByRole("button", { name: /Best of/ }).querySelector("img")).toHaveAttribute("src", "/v1/instagram/s/9/x/h"));
    fireEvent.click(screen.getByRole("tab", { name: "Posts" }));
    fireEvent.click(screen.getByRole("tab", { name: "Highlights" }));
    expect(calls.filter((c) => c.url === "/v1/instagram/highlights/natgeo")).toHaveLength(1);
  });

  it("uses the tray route when the bundle is empty and renders the cover", async () => {
    const calls = serve({ profile: bundle(), highlights: bundle({ highlights: true }).highlights });
    renderIg("/instagram/natgeo");
    fireEvent.click(await screen.findByRole("tab", { name: "Highlights" }));
    const highlight = await screen.findByRole("button", { name: /Best of/ });
    expect(highlight.querySelector("img")).toHaveAttribute("src", "/v1/instagram/s/9/x/h");
    expect(calls.filter((c) => c.url === "/v1/instagram/highlights/natgeo")).toHaveLength(1);
    fireEvent.click(screen.getByRole("tab", { name: "Posts" }));
    fireEvent.click(screen.getByRole("tab", { name: "Highlights" }));
    expect(calls.filter((c) => c.url === "/v1/instagram/highlights/natgeo")).toHaveLength(1);
  });

  it("retries an empty tray on reentry and with Try again, without looping", async () => {
    const r: Routes = { profile: bundle() };
    const calls = serve(r);
    renderIg("/instagram/natgeo");
    fireEvent.click(await screen.findByRole("tab", { name: "Highlights" }));
    await screen.findByText("No highlights.");
    expect(calls.filter((c) => c.url === "/v1/instagram/highlights/natgeo")).toHaveLength(1);
    fireEvent.click(screen.getByRole("tab", { name: "Posts" }));
    fireEvent.click(screen.getByRole("tab", { name: "Highlights" }));
    await screen.findByText("No highlights.");
    expect(calls.filter((c) => c.url === "/v1/instagram/highlights/natgeo")).toHaveLength(2);
    r.highlights = bundle({ highlights: true }).highlights;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("button", { name: /Best of/ })).toBeInTheDocument();
    expect(calls.filter((c) => c.url === "/v1/instagram/highlights/natgeo")).toHaveLength(3);
  });

  it("shows tray errors and can retry", async () => {
    const r: Routes = { profile: bundle(), highlightsStatus: 502 };
    serve(r);
    renderIg("/instagram/natgeo");
    fireEvent.click(await screen.findByRole("tab", { name: "Highlights" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't load this right now.");
    expect(screen.queryByText("No highlights.")).toBeNull();
    r.highlightsStatus = undefined;
    r.highlights = bundle({ highlights: true }).highlights;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("button", { name: /Best of/ })).toBeInTheDocument();
  });

  it("does not request the tray for a private profile", async () => {
    const calls = serve({ profile: bundle({ isPrivate: true }) });
    renderIg("/instagram/natgeo");
    fireEvent.click(await screen.findByRole("tab", { name: "Highlights" }));
    expect(screen.getByText("No highlights.")).toBeInTheDocument();
    expect(calls.some((c) => c.url === "/v1/instagram/highlights/natgeo")).toBe(false);
  });

  describe("a highlight's tiles", () => {
    const ids = ["h1", "h2", "h3", "h4", "h5", "h6"];
    const tiles = () => document.querySelectorAll<HTMLElement>(".ig-tiles .ig-hl__tile");
    async function openHighlight() {
      serve({ profile: bundle(), highlights: bundle({ highlights: true }).highlights, highlightItems: { items: [...ids.map((i) => item(i)), item("v7", "video")], nextCursor: null } });
      renderIg("/instagram/natgeo");
      fireEvent.click(await screen.findByRole("tab", { name: "Highlights" }));
      fireEvent.click(await screen.findByRole("button", { name: /Best of/ }));
      return screen.findByRole("dialog", { name: "Best of: all items" });
    }
    const viewer = () => screen.queryByRole("dialog", { name: /^\d+ of 7$/ });

    it("opens on one tile per item, with the title, the count and a video badge", async () => {
      const grid = await openHighlight();
      await waitFor(() => expect(tiles()).toHaveLength(7));
      expect(grid).toHaveTextContent("Best of");
      expect(grid).toHaveTextContent("@natgeo · 7 items");
      // The highlight's cover heads the grid; no heading per day.
      expect(grid.querySelector(".ig-hl__cover img")).toHaveAttribute("src", "/v1/instagram/s/9/x/h");
      expect(grid.querySelectorAll("h2, h3")).toHaveLength(1);
      expect(tiles()[6]!.querySelector(".ig-tile__badge")).not.toBeNull();
      expect(tiles()[0]!.querySelector(".ig-tile__badge")).toBeNull();
      expect(tiles()[0]!.querySelector("img")).toHaveAttribute("loading", "lazy");
      expect(tiles()[0]!.querySelector("img")).toHaveAttribute("src", "/v1/instagram/s/9/x/h1-t");
      expect(viewer()).toBeNull();
    });

    it("opens the viewer on the tile that was clicked", async () => {
      await openHighlight();
      await waitFor(() => expect(tiles()).toHaveLength(7));
      fireEvent.click(tiles()[3]!);
      expect(await screen.findByRole("dialog", { name: "4 of 7" })).toBeInTheDocument();
      expect(screen.queryByRole("dialog", { name: "Best of: all items" })).toBeNull();
    });

    it("keeps the tiles under the viewer, so closing it comes back to the same place and tile", async () => {
      await openHighlight();
      await waitFor(() => expect(tiles()).toHaveLength(7));
      const panel = document.querySelector<HTMLElement>(".ig-tiles")!;
      const scroller = panel.querySelector<HTMLElement>(".ig-hl__scroll")!;
      scroller.scrollTop = 300;
      fireEvent.click(tiles()[4]!);
      await screen.findByRole("dialog", { name: "5 of 7" });
      expect(document.querySelector(".ig-tiles")).toBe(panel);
      expect(panel).toHaveAttribute("inert");
      fireEvent.keyDown(window, { key: "ArrowRight" });
      await screen.findByRole("dialog", { name: "6 of 7" });
      fireEvent.keyDown(window, { key: "Escape" });
      expect(await screen.findByRole("dialog", { name: "Best of: all items" })).toBe(panel);
      expect(panel).not.toHaveAttribute("inert");
      expect(scroller.scrollTop).toBe(300);
      expect(document.activeElement).toBe(tiles()[5]);
    });

    it("toggles between the tiles and the viewer with G, and Esc steps back out", async () => {
      await openHighlight();
      await waitFor(() => expect(tiles()).toHaveLength(7));
      fireEvent.keyDown(window, { key: "g" });
      expect(await screen.findByRole("dialog", { name: "1 of 7" })).toBeInTheDocument();
      fireEvent.keyDown(window, { key: "ArrowRight" });
      expect(await screen.findByRole("dialog", { name: "2 of 7" })).toBeInTheDocument();
      fireEvent.keyDown(window, { key: "G" });
      expect(await screen.findByRole("dialog", { name: "Best of: all items" })).toBeInTheDocument();
      expect(viewer()).toBeNull();
      // Back on the slide the viewer was on.
      fireEvent.keyDown(window, { key: "g" });
      expect(await screen.findByRole("dialog", { name: "2 of 7" })).toBeInTheDocument();
      fireEvent.keyDown(window, { key: "Escape" });
      expect(await screen.findByRole("dialog", { name: "Best of: all items" })).toBeInTheDocument();
      fireEvent.keyDown(window, { key: "Escape" });
      await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    });

    it("goes back to the tiles from the viewer's grid button", async () => {
      await openHighlight();
      await waitFor(() => expect(tiles()).toHaveLength(7));
      fireEvent.click(tiles()[5]!);
      await screen.findByRole("dialog", { name: "6 of 7" });
      fireEvent.click(screen.getByRole("button", { name: "All items as tiles (G)" }));
      expect(await screen.findByRole("dialog", { name: "Best of: all items" })).toBeInTheDocument();
    });

    it("moves between tiles with the arrow keys", async () => {
      await openHighlight();
      await waitFor(() => expect(tiles()).toHaveLength(7));
      tiles()[0]!.focus();
      fireEvent.keyDown(tiles()[0]!, { key: "ArrowRight" });
      expect(document.activeElement).toBe(tiles()[1]);
      fireEvent.keyDown(tiles()[1]!, { key: "End" });
      expect(document.activeElement).toBe(tiles()[6]);
      fireEvent.keyDown(tiles()[6]!, { key: "ArrowLeft" });
      expect(document.activeElement).toBe(tiles()[5]);
    });
  });

  it("opens a story in the viewer, with a grid button to its tiles", async () => {
    serve({ profile: bundle({ hasStories: true }), stories: { items: [item("s1"), item("s2"), item("s3")], nextCursor: null } });
    renderIg("/instagram/natgeo");
    const tile = await screen.findByRole("button", { name: /Stories 2/ });
    fireEvent.click(tile);
    expect(await screen.findByRole("dialog", { name: "2 of 3" })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "g" });
    expect(await screen.findByRole("dialog", { name: "@natgeo: all items" })).toBeInTheDocument();
  });

  it("shows a highlight's count only when the server knows it", async () => {
    const calls = serve({ profile: bundle({ highlights: true }) });
    renderIg("/instagram/natgeo");
    fireEvent.click(await screen.findByRole("tab", { name: "Highlights" }));
    expect(screen.getByRole("button", { name: /Best of/ })).toHaveTextContent("2 items");
    expect(calls.some((c) => c.url === "/v1/instagram/highlights/natgeo")).toBe(false);
  });

  it("shows no count for a highlight whose mediaCount is null", async () => {
    serve({ profile: bundle({ highlights: true, highlightCount: null }) });
    renderIg("/instagram/natgeo");
    fireEvent.click(await screen.findByRole("tab", { name: "Highlights" }));
    const button = screen.getByRole("button", { name: /Best of/ });
    expect(button).toHaveTextContent(/^Best of$/);
    expect(button.querySelector(".ig-highlight__count")).toBeNull();
  });

  it("explains a private account, a missing one and a rate limit", async () => {
    serve({ profile: bundle({ isPrivate: true }) });
    renderIg("/instagram/natgeo");
    expect(await screen.findByText(/This account is private/)).toBeInTheDocument();
  });

  it("says when the account is not found, with no retry button", async () => {
    serve({ profileStatus: 404 });
    renderIg("/instagram/ghost");
    expect(await screen.findByRole("alert")).toHaveTextContent("No account with that name");
    expect(screen.queryByRole("button", { name: "Try again" })).toBeNull();
  });

  it("builds messages for 429 and other failures", async () => {
    const { ApiError } = await import("../api/client");
    expect(errorText(new ApiError(429, "x", "x", 12))).toBe("Instagram asks to slow down. Try again in 12 s.");
    expect(errorText(new ApiError(429, "x", "x"))).toBe("Instagram asks to slow down. Try again in 60 s.");
    expect(errorText(new ApiError(502, "x", "x"))).toBe("Couldn't load this right now.");
    expect(errorText(new Error("boom"))).toBe("Couldn't load this right now.");
  });

  it("resolves a pasted post link and shows it in the viewer", async () => {
    const calls = serve({ resolve: { items: [item("DeXzbFEl5DG")], nextCursor: null } });
    renderIg();
    fireEvent.input(screen.getByRole("searchbox"), { target: { value: "https://www.instagram.com/natgeo/p/DeXzbFEl5DG/" } });
    fireEvent.click(screen.getByRole("button", { name: "Open" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog.querySelector(".pv-name")).toHaveTextContent("@natgeo");
    expect(calls.some((c) => c.url.startsWith("/v1/instagram/resolve?url="))).toBe(true);
  });

  describe("watching", () => {
    const entry = (over: Record<string, unknown> = {}) => ({ username: "natgeo", autoSave: true, lastCheck: 1, newStories: 2, newPosts: 1, private: false, fullName: "National Geographic", picUrl: null, ...over });

    it("shows the watched accounts as chips with their red dots", async () => {
      serve({ watched: [entry(), entry({ username: "nasa", newStories: 0, newPosts: 0 })] });
      renderIg();
      const chip = await screen.findByRole("button", { name: "@natgeo, 3 new" });
      expect(within(chip).getByText("3")).toBeInTheDocument();
      const quiet = screen.getByRole("button", { name: "@nasa" });
      expect(quiet.querySelector(".ig-dot")).toBeNull();
    });

    it("opens a chip's profile and clears its badge", async () => {
      const setBadge = vi.fn();
      (window as { secretaryShell?: unknown }).secretaryShell = { showCode() {}, setBadge };
      const calls = serve({ watched: [entry()], profile: bundle(), opened: { total: 4, accounts: { nasa: 4 } } });
      renderIg();
      fireEvent.click(await screen.findByRole("button", { name: "@natgeo, 3 new" }));
      await screen.findByRole("heading", { name: /National Geographic/ });
      await waitFor(() => expect(calls.some((c) => c.method === "POST" && c.url === "/v1/instagram/watch/natgeo/opened")).toBe(true));
      await waitFor(() => expect(instagramBadges().total).toBe(4));
      expect(setBadge).toHaveBeenCalledWith("instagram", 4);
      expect(calls.find((c) => c.url.endsWith("/opened"))!.headers["x-secretary-csrf"]).toBe("1");
    });

    it("does not call opened for an account that is not watched", async () => {
      const calls = serve({ profile: bundle() });
      renderIg("/instagram/natgeo");
      await screen.findByRole("heading", { name: /National Geographic/ });
      expect(screen.getByRole("button", { name: "Watch" })).toBeInTheDocument();
      expect(calls.some((c) => c.url.endsWith("/opened"))).toBe(false);
    });

    it("starts watching from the profile, switches auto-save and stops", async () => {
      let list: unknown[] = [];
      const calls = stubFetch((call) => {
        const url = call.url;
        if (url.startsWith("/v1/instagram/profile/")) return { body: { ok: true, data: bundle() } };
        if (url === "/v1/instagram/watch" && call.method === "GET") return { body: { ok: true, data: list } };
        if (url === "/v1/instagram/watch/natgeo" && call.method === "PUT") {
          list = [entry({ newStories: 0, newPosts: 0, autoSave: (call.body as { autoSave?: boolean }).autoSave ?? true })];
          return { body: { ok: true, data: list[0] } };
        }
        if (url === "/v1/instagram/watch/natgeo" && call.method === "DELETE") {
          list = [];
          return { body: { ok: true, data: { removed: true } } };
        }
        if (url.endsWith("/opened")) return { body: { ok: true, data: { total: 0, accounts: {} } } };
        if (url === "/v1/instagram/badges") return { body: { ok: true, data: { total: 0, accounts: {} } } };
        return { status: 404, body: {} };
      });
      renderIg("/instagram/natgeo");
      fireEvent.click(await screen.findByRole("button", { name: "Watch" }));
      expect(await screen.findByRole("button", { name: "Watching" })).toHaveAttribute("aria-pressed", "true");
      const put = calls.find((c) => c.method === "PUT")!;
      expect(put.headers["x-secretary-csrf"]).toBe("1");
      const box = screen.getByRole("checkbox", { name: "Save new stories to Lucina" });
      expect(box).toBeChecked();
      fireEvent.click(box);
      await waitFor(() => expect(calls.filter((c) => c.method === "PUT").at(-1)!.body).toEqual({ autoSave: false }));
      await waitFor(() => expect(screen.getByRole("checkbox", { name: "Save new stories to Lucina" })).not.toBeChecked());
      fireEvent.click(screen.getByRole("button", { name: "Watching" }));
      expect(await screen.findByRole("button", { name: "Watch" })).toBeInTheDocument();
      expect(calls.some((c) => c.method === "DELETE" && c.url === "/v1/instagram/watch/natgeo")).toBe(true);
    });

    it("says a watched account is checked about once a day, and when next", async () => {
      vi.spyOn(Date, "now").mockReturnValue(1_800_000_000_000);
      serve({ watched: [entry({ nextCheck: 1_800_000_000 + 22 * 3600 })], profile: bundle() });
      renderIg("/instagram/natgeo");
      const hint = await screen.findByText(/Checked about once a day\. Nothing is marked as seen on Instagram\./);
      // A clock time, not a countdown that goes stale on screen.
      expect(hint).toHaveTextContent(/Next check: (today|tomorrow) \d\d:\d\d( [AP]M)?\./);
      expect(hint).toHaveTextContent("Applies to all watched accounts.");
      const select = screen.getByRole("combobox", { name: "Check" }) as HTMLSelectElement;
      expect(select).toHaveAccessibleDescription("Applies to all watched accounts.");
      expect([...select.options].map((o) => o.textContent)).toEqual(["About once a day", "Every 12 hours", "Every 6 hours"]);
      expect(select.value).toBe("23");
    });

    it("says the first check comes in a moment, then asks again for the next one", async () => {
      const now = Date.now();
      const calls = serve({ watched: [entry({ lastCheck: null, nextCheck: Math.floor(now / 1000) + 5 })], profile: bundle() });
      renderIg("/instagram/natgeo");
      const hint = await screen.findByText(/Checked about once a day/);
      expect(hint).toHaveTextContent("First check: in a moment.");
      expect(hint).not.toHaveTextContent("seconds");
      const asked = () => calls.filter((c) => c.url === "/v1/instagram/watch").length;
      const before = asked();
      await waitFor(() => expect(asked()).toBeGreaterThan(before), { timeout: 15_000 });
    }, 20_000);

    it("says every 12 hours when that is the setting", async () => {
      serve({ watched: [entry()], profile: bundle(), intervalHours: 12 });
      renderIg("/instagram/natgeo");
      expect(await screen.findByText(/Checked about every 12 hours\. Nothing is marked as seen on Instagram\./)).toBeInTheDocument();
      expect((screen.getByRole("combobox", { name: "Check" }) as HTMLSelectElement).value).toBe("12");
    });

    it("choosing every 12 hours sends the setting", async () => {
      const calls = serve({ watched: [entry()], profile: bundle() });
      renderIg("/instagram/natgeo");
      await screen.findByText(/Checked about once a day/);
      const select = screen.getByRole("combobox", { name: "Check" });
      fireEvent.change(select, { target: { value: "12" } });
      expect(await screen.findByText(/Checked about every 12 hours/)).toBeInTheDocument();
      const put = calls.find((c) => c.method === "PUT" && c.url === "/v1/instagram/watch-settings")!;
      expect(put.body).toEqual({ intervalHours: 12 });
      expect(put.headers["x-secretary-csrf"]).toBe("1");
    });

    it("tells the owner when the setting could not be changed", async () => {
      serve({ watched: [entry()], profile: bundle(), settingsStatus: 502 });
      renderIg("/instagram/natgeo");
      await screen.findByText(/Checked about once a day/);
      const select = screen.getByRole("combobox", { name: "Check" }) as HTMLSelectElement;
      fireEvent.change(select, { target: { value: "6" } });
      expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't change how often accounts are checked.");
      expect(select.value).toBe("23");
      expect(screen.getByText(/Checked about once a day/)).toBeInTheDocument();
    });

    it("tells the owner when a watched account stopped answering", async () => {
      serve({ watched: [entry({ gone: true })], profile: bundle() });
      renderIg("/instagram/natgeo");
      expect(await screen.findByText(/no longer answers/)).toBeInTheDocument();
    });

    it("lists the Archive, opens it in the viewer without save buttons", async () => {
      const calls = serve({
        profile: bundle(),
        archive: { items: [item("a1", "image"), item("a2", "video")], nextCursor: null },
      });
      renderIg("/instagram/natgeo");
      fireEvent.click(await screen.findByRole("tab", { name: "Archive" }));
      await waitFor(() => expect(screen.getAllByRole("button", { name: /^Archive \d/ })).toHaveLength(2));
      expect(calls.some((c) => c.url === "/v1/instagram/archive/natgeo")).toBe(true);
      fireEvent.click(screen.getByRole("button", { name: /^Archive 1/ }));
      const dialog = await screen.findByRole("dialog");
      expect(dialog).toHaveAttribute("aria-label", "1 of 2");
      expect(within(dialog).queryByRole("button", { name: /Save/ })).toBeNull();
    });

    it("says when nothing was saved yet", async () => {
      serve({ profile: bundle() });
      renderIg("/instagram/natgeo");
      fireEvent.click(await screen.findByRole("tab", { name: "Archive" }));
      expect(await screen.findByText("Nothing saved from this account yet.")).toBeInTheDocument();
    });
  });

  describe("the viewer", () => {
    async function openFirst(extra: Routes = {}) {
      const calls = serve({ profile: bundle(), ...extra });
      renderIg("/instagram/natgeo");
      fireEvent.click(await screen.findByRole("button", { name: /^Posts 1/ }));
      return { calls, dialog: await screen.findByRole("dialog") };
    }

    it("moves with the arrow keys and closes with Escape", async () => {
      const { dialog } = await openFirst();
      expect(dialog).toHaveAttribute("aria-label", "1 of 3");
      expect(within(dialog).getByText("caption p1")).toBeInTheDocument();
      fireEvent.keyDown(window, { key: "ArrowRight" });
      expect(dialog).toHaveAttribute("aria-label", "2 of 3");
      expect(within(dialog).getByText("caption p2")).toBeInTheDocument();
      fireEvent.keyDown(window, { key: "ArrowLeft" });
      fireEvent.keyDown(window, { key: "ArrowLeft" });
      expect(dialog).toHaveAttribute("aria-label", "1 of 3");
      fireEvent.keyDown(window, { key: "Escape" });
      expect(screen.queryByRole("dialog")).toBeNull();
    });

    it("swipes", async () => {
      const { dialog } = await openFirst();
      const stage = dialog.querySelector<HTMLElement>(".pv-stage")!;
      const drag = (from: [number, number], to: [number, number]) => {
        fireEvent.pointerDown(stage, { pointerId: 1, clientX: from[0], clientY: from[1] });
        fireEvent.pointerMove(stage, { pointerId: 1, clientX: to[0], clientY: to[1] });
        fireEvent.pointerUp(stage, { pointerId: 1, clientX: to[0], clientY: to[1] });
      };
      drag([300, 400], [120, 410]);
      await waitFor(() => expect(dialog).toHaveAttribute("aria-label", "2 of 3"));
      drag([100, 400], [260, 380]);
      await waitFor(() => expect(dialog).toHaveAttribute("aria-label", "1 of 3"));
      // A mostly vertical drag upwards is not a swipe (nor a close).
      drag([100, 300], [95, 200]);
      await new Promise((r) => setTimeout(r, 300));
      expect(screen.getByRole("dialog")).toHaveAttribute("aria-label", "1 of 3");
    });

    it("plays a video slide", async () => {
      const calls = serve({ profile: { ...bundle(), posts: { items: [item("v1", "video")], nextCursor: null } } });
      renderIg("/instagram/natgeo");
      fireEvent.click(await screen.findByRole("button", { name: /^Posts 1/ }));
      const dialog = await screen.findByRole("dialog");
      const video = dialog.querySelector("video");
      expect(video).not.toBeNull();
      expect(video!.getAttribute("src")).toBe("/v1/instagram/s/9/x/v1");
      expect(video!.hasAttribute("controls")).toBe(true);
      expect(video!.autoplay && video!.loop && video!.muted).toBe(true);
      expect(video!.hasAttribute("playsinline")).toBe(true);
      expect(calls).toBeTruthy();
    });

    it("saves the current slide and tells the owner", async () => {
      const { calls, dialog } = await openFirst();
      fireEvent.click(within(dialog).getByRole("button", { name: "Save to Lucina" }));
      expect(await screen.findByText("Saved to Lucina", { selector: ".ig-toast" })).toBeInTheDocument();
      const save = calls.find((c) => c.url === "/v1/instagram/save")!;
      expect(save.method).toBe("POST");
      expect(save.headers["x-secretary-csrf"]).toBe("1");
      expect(save.body).toEqual({ items: [{ url: "/v1/instagram/s/9/x/p1", kind: "image", username: "natgeo", takenAt: 1_700_000_000, caption: "caption p1" }] });
    });

    it("says it is a duplicate", async () => {
      const { dialog } = await openFirst({ save: { saved: [{ index: 0, itemId: "itm_1", duplicate: true }], failed: [] } });
      fireEvent.click(within(dialog).getByRole("button", { name: "Save to Lucina" }));
      expect(await screen.findByText("Already in Lucina", { selector: ".ig-toast" })).toBeInTheDocument();
    });

    it("reports a failed save and lets the owner try again", async () => {
      const { dialog } = await openFirst({ save: { saved: [], failed: [{ index: 0, error: "expired" }] } });
      fireEvent.click(within(dialog).getByRole("button", { name: "Save to Lucina" }));
      expect(await screen.findByText(/Couldn't save: the link expired/)).toBeInTheDocument();
      expect(within(dialog).getByRole("button", { name: "Save to Lucina" })).toBeEnabled();
    });

    it("saves every frame of a carousel", async () => {
      const carousel: IgItem = { ...item("c1", "carousel"), children: [
        { id: "c1_0", kind: "image", url: "/v1/instagram/s/9/x/c0", thumbUrl: null },
        { id: "c1_1", kind: "video", url: "/v1/instagram/s/9/x/c1", thumbUrl: null },
      ] };
      const calls = serve({ profile: { ...bundle(), posts: { items: [carousel], nextCursor: null } }, save: { saved: [{ index: 0, itemId: "a", duplicate: false }, { index: 1, itemId: "b", duplicate: false }], failed: [] } });
      renderIg("/instagram/natgeo");
      fireEvent.click(await screen.findByRole("button", { name: /^Posts 1/ }));
      const dialog = await screen.findByRole("dialog");
      expect(dialog).toHaveAttribute("aria-label", "1 of 2");
      fireEvent.click(within(dialog).getByRole("button", { name: "Save all 2" }));
      expect(await screen.findByText("Saved 2 to Lucina", { selector: ".ig-toast" })).toBeInTheDocument();
      const body = calls.find((c) => c.url === "/v1/instagram/save")!.body as { items: { url: string; kind: string }[] };
      expect(body.items.map((i) => [i.url, i.kind])).toEqual([["/v1/instagram/s/9/x/c0", "image"], ["/v1/instagram/s/9/x/c1", "video"]]);
    });
  });
});

describe("Instagram desktop", () => {
  beforeEach(() => stubWide());
  afterEach(() => {
    vi.unstubAllGlobals();
    resetInstagramBadges();
  });

  it("shows the header, Open accent, recent list and watched rail", async () => {
    serve({
      recent: [{ username: "nasa", fullName: "NASA", isVerified: true, picUrl: null, openedAt: 1 }],
      watched: [{ username: "natgeo", picUrl: null, newStories: 2, newPosts: 0, autoSave: false, gone: false }],
    });
    renderIg();
    expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent("Instagram");
    expect(screen.getByText("Anonymous: nothing here logs in to Instagram.")).toBeInTheDocument();
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(1);
    expect(screen.getByRole("region", { name: "Recent profiles" })).toBeInTheDocument();
    expect(screen.getByRole("complementary", { name: "Watched accounts" })).toBeInTheDocument();
    expect(document.querySelector(".ig-chips")).toBeNull();
  });

  it("shows a back link on a profile", async () => {
    serve({ profile: bundle() });
    renderIg("/instagram/natgeo");
    expect(await screen.findByRole("link", { name: "Recent profiles" })).toHaveAttribute("href", "/instagram");
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Instagram");
  });
});
