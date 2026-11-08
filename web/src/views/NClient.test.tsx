import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { savePoll } from "../components/NClientSave";
import { setShowAdult } from "../lib/adult";
import { deferred, stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
import NClientView from "./NClient";

const image = (id: number, kind = "thumb", index = 0) => `/v1/nclient/s/${id}/${kind}/${index}/123/abc123`;
const summary = { id: 7, title: "Placeholder volume", pages: 6, language: "english", tags: [], thumbUrl: image(7), favorite: false, progress: null };
const pages = (n: number) => Array.from({ length: n }, (_, index) => ({ index, width: 800, height: 1200, url: image(7, "page", index), thumbUrl: image(7, "pthumb", index) }));
const detail = {
  ...summary, coverUrl: image(7, "cover"), pages: pages(6),
  tags: [{ id: 11, name: "color", type: "tag", count: 1 }],
  related: [{ ...summary, id: 8, title: "Related volume", thumbUrl: image(8) }],
  progress: { page: 1, pages: 6, updated_at: 0 },
};

interface Options { enabled?: boolean; failSearch?: boolean; failSave?: boolean; saveStates?: Record<string, unknown>[]; saveStart?: { status?: number; body?: unknown }; pageCount?: number }
function serve(options: Options = {}) {
  const states = [...(options.saveStates ?? [])];
  const galleryDetail = () => ({ ...detail, pages: pages(options.pageCount ?? 6) });
  return stubFetch(c => {
    let data: unknown = {};
    if (c.url === "/v1/nclient/status") data = { enabled: options.enabled ?? true, save: { gallery: true, library: false } };
    else if (c.url.startsWith("/v1/nclient/search")) {
      if (options.failSearch) return { status: 500 };
      data = { items: [summary], page: Number(new URL(c.url, "http://localhost").searchParams.get("page") ?? 1), numPages: 2 };
    } else if (c.url.startsWith("/v1/nclient/tags")) data = { tags: [{ id: 11, name: "color", type: "tag", count: 1 }, { id: 12, name: "long", type: "tag", count: 1 }] };
    else if (c.url === "/v1/nclient/galleries/7" || c.url === "/v1/nclient/random") data = galleryDetail();
    else if (c.url === "/v1/nclient/galleries/8") data = { ...galleryDetail(), id: 8, title: "Related volume" };
    else if (c.url === "/v1/nclient/favorites") data = { items: [summary] };
    else if (c.url === "/v1/nclient/favorites/7") data = { favorite: c.method === "PUT" };
    else if (c.url === "/v1/nclient/progress/7") {
      if (options.failSave) return { status: 500 };
      data = { page: 1 };
    } else if (c.url === "/v1/nclient/galleries/7/save") {
      if (options.saveStart) return { status: options.saveStart.status ?? 202, body: options.saveStart.body };
      data = { job: "job1", state: "queued", total: 6, done: 0, saved: [], failed: [] };
      return { status: 202, body: { ok: true, data } };
    } else if (c.url === "/v1/nclient/saves/job1") {
      if (c.method === "DELETE") {
        // The service stops after the page it is on.
        states.splice(0, states.length, { job: "job1", state: "cancelled", total: 6, done: 2, saved: [{ index: 0, itemId: "a", duplicate: false }, { index: 1, itemId: "b", duplicate: false }], failed: [] });
        return { body: { ok: true, data: { cancelling: true } } };
      }
      data = states.length > 1 ? states.shift() : states[0];
    } else return { status: 404 };
    return { body: { ok: true, data } };
  });
}
function renderNClient() { return renderAt("/nclient", () => <Route path="/nclient" component={NClientView} />); }
async function openDetail() { fireEvent.click(await screen.findByRole("button", { name: /^Placeholder volume/ })); await screen.findByRole("heading", { name: "Placeholder volume" }); }
async function openReader() { await openDetail(); fireEvent.click(await screen.findByRole("button", { name: "Resume" })); }

describe("NClient", () => {
  beforeEach(() => { setShowAdult(true); savePoll.ms = 5; });
  afterEach(() => { vi.restoreAllMocks(); setShowAdult(true); });

  it("does not fetch content when disabled, including direct route visits", async () => {
    const calls = serve({ enabled: false }); renderNClient();
    expect(await screen.findByText("NClient is unavailable")).toBeInTheDocument();
    expect(calls.map(c => c.url)).toEqual(["/v1/nclient/status"]);
  });

  it("keeps a failed status closed", async () => {
    const calls = stubFetch(() => ({ status: 503 })); renderNClient();
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not load NClient");
    expect(calls).toHaveLength(1);
  });

  it("fetches nothing while adult plugins are hidden on this device, and shows them again on request", async () => {
    setShowAdult(false);
    const calls = serve(); renderNClient();
    expect(await screen.findByText("Adult plugins are hidden on this device.")).toBeInTheDocument();
    expect(calls).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Show adult plugins" }));
    expect(await screen.findByRole("button", { name: /^Placeholder volume/ })).toBeInTheDocument();
    expect(calls[0]!.url).toBe("/v1/nclient/status");
  });

  it("searches with sort and selected include/exclude tags and paginates", async () => {
    const calls = serve(); renderNClient();
    await screen.findByRole("button", { name: /^Placeholder volume/ });
    fireEvent.input(screen.getByRole("searchbox", { name: "Search galleries" }), { target: { value: "test & query" } });
    fireEvent.change(screen.getByLabelText("Sort"), { target: { value: "popular-month" } });
    fireEvent.click(screen.getByText("Tag filters"));
    fireEvent.input(screen.getByRole("searchbox", { name: "Find tags" }), { target: { value: "c" } });
    fireEvent.click(await screen.findByRole("button", { name: "Include color" }, { timeout: 2000 }));
    fireEvent.click(screen.getByRole("button", { name: "Exclude long" }));
    // Chosen tags leave the picker and show as chips that can be removed.
    expect(screen.queryByRole("button", { name: "Include color" })).toBeNull();
    expect(screen.getByRole("button", { name: "Remove: +color" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Remove: −long" })).toBeInTheDocument();
    fireEvent.submit(screen.getByRole("searchbox", { name: "Search galleries" }).closest("form")!);
    await waitFor(() => expect(calls.some(c => c.url.includes("tags=11&exclude=12"))).toBe(true));
    const query = new URL(calls.find(c => c.url.includes("tags=11"))!.url, "http://localhost").searchParams;
    expect(query.get("q")).toBe("test & query"); expect(query.get("sort")).toBe("popular-month");
    await screen.findByRole("button", { name: /^Placeholder volume/ });
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    await waitFor(() => expect(calls.some(c => c.url.includes("page=2"))).toBe(true));
    expect(window.location.search).toBe("");
    expect(window.location.pathname).not.toMatch(/\d/);
  });

  it("loads favorites, detail, related and toggles favorites", async () => {
    const calls = serve(); renderNClient();
    fireEvent.click(await screen.findByRole("button", { name: "Favorites" }));
    await waitFor(() => expect(calls.some(c => c.url === "/v1/nclient/favorites")).toBe(true));
    await openDetail();
    fireEvent.click(await screen.findByRole("button", { name: "Add to favorites" }));
    fireEvent.click(await screen.findByRole("button", { name: "Remove from favorites" }));
    await screen.findByRole("button", { name: "Add to favorites" });
    expect(calls.filter(c => c.url === "/v1/nclient/favorites/7").map(c => c.method)).toEqual(["PUT", "DELETE"]);
    fireEvent.click(screen.getByRole("button", { name: /^Related volume/ }));
    expect(await screen.findByRole("heading", { name: "Related volume" })).toBeInTheDocument();
  });

  it("shows page thumbnails on the detail and opens the reader at the chosen page", async () => {
    serve({ pageCount: 30 }); renderNClient();
    await openDetail();
    const thumbs = screen.getAllByRole("button", { name: /^Open page/ });
    expect(thumbs).toHaveLength(24);
    expect(thumbs[3]!.querySelector("img")).toHaveAttribute("src", image(7, "pthumb", 3));
    fireEvent.click(screen.getByRole("button", { name: "Show 6 more pages" }));
    expect(screen.getAllByRole("button", { name: /^Open page/ })).toHaveLength(30);
    fireEvent.click(screen.getByRole("button", { name: "Open page 5" }));
    expect(await screen.findByRole("img", { name: "Page 5 of 30" })).toHaveAttribute("src", image(7, "page", 4));
  });

  it("resumes, preloads only the next three pages once the current one is shown, saves progress, and keeps content out of history and storage", async () => {
    const calls = serve(); const push = vi.spyOn(window.history, "pushState"); const replace = vi.spyOn(window.history, "replaceState");
    renderNClient(); await openReader();
    const current = screen.getByRole("img", { name: "Page 2 of 6" });
    expect(current).toHaveAttribute("src", image(7, "page", 1));
    // Nothing is fetched ahead until the current page has loaded.
    expect(document.querySelectorAll("[data-nclient-preload] img")).toHaveLength(0);
    fireEvent.load(current);
    expect(Array.from(document.querySelectorAll("[data-nclient-preload] img")).map(i => i.getAttribute("src"))).toEqual([2, 3, 4].map(i => image(7, "page", i)));
    fireEvent.keyDown(window, { key: "ArrowRight" });
    expect(screen.getByRole("img", { name: "Page 3 of 6" })).toBeInTheDocument();
    expect(document.querySelectorAll("[data-nclient-preload] img")).toHaveLength(0);
    fireEvent.keyDown(window, { key: "ArrowLeft" });
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    fireEvent.change(screen.getByLabelText("Reading page"), { target: { value: "6" } });
    await waitFor(() => expect(calls.filter(c => c.url.includes("/progress/7")).at(-1)?.body).toEqual({ page: 5, pages: 6 }));
    expect(screen.getByRole("button", { name: "Next" })).toBeDisabled();
    fireEvent.change(screen.getByLabelText("Fit"), { target: { value: "width" } });
    fireEvent.input(screen.getByLabelText("Zoom"), { target: { value: "150" } });
    expect(screen.getByRole("img", { name: "Page 6 of 6" }).className).toContain("nclient-page--width");
    for (const img of document.querySelectorAll(".nclient img")) expect(img).toHaveAttribute("referrerpolicy", "no-referrer");
    expect(document.title).toBe("NClient"); expect(push).not.toHaveBeenCalled();
    expect(replace.mock.calls.every(c => c[2] === undefined || !/\d/.test(String(c[2])))).toBe(true);
    expect(Object.keys(localStorage).filter(k => k !== "secretary.showAdultPlugins")).toEqual([]);
    expect(sessionStorage.length).toBe(0);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.getByRole("button", { name: "Resume" })).toBeInTheDocument();
  });

  it("zooms with the buttons, the keys and a Ctrl + wheel", async () => {
    serve(); renderNClient(); await openReader();
    const zoom = screen.getByLabelText("Zoom") as HTMLInputElement;
    expect(zoom.value).toBe("100");
    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    expect(zoom.value).toBe("125");
    fireEvent.keyDown(window, { key: "+" });
    expect(zoom.value).toBe("150");
    fireEvent.keyDown(window, { key: "-" });
    fireEvent.click(screen.getByRole("button", { name: "Zoom out" }));
    expect(zoom.value).toBe("100");
    const viewport = document.querySelector(".nclient-viewport")!;
    fireEvent.wheel(viewport, { deltaY: -100, ctrlKey: true });
    expect(zoom.value).toBe("110");
    fireEvent.wheel(viewport, { deltaY: -100 }); // a plain wheel scrolls, it does not zoom
    expect(zoom.value).toBe("110");
  });

  it("supports swipe paging, pinch zoom, and ignores keyboard paging in inputs", async () => {
    serve(); renderNClient(); await openReader();
    const viewport = document.querySelector(".nclient-viewport")!;
    fireEvent.touchStart(viewport, { touches: [{ clientX: 200, clientY: 20 }] });
    fireEvent.touchEnd(viewport, { changedTouches: [{ clientX: 50, clientY: 25 }], touches: [] });
    expect(screen.getByRole("img", { name: "Page 3 of 6" })).toBeInTheDocument();
    // A pinch zooms and does not turn the page.
    fireEvent.touchStart(viewport, { touches: [{ clientX: 100, clientY: 100 }, { clientX: 200, clientY: 100 }] });
    fireEvent.touchMove(viewport, { touches: [{ clientX: 50, clientY: 100 }, { clientX: 250, clientY: 100 }] });
    expect((screen.getByLabelText("Zoom") as HTMLInputElement).value).toBe("200");
    fireEvent.touchEnd(viewport, { changedTouches: [{ clientX: 50, clientY: 100 }], touches: [{ clientX: 250, clientY: 100 }] });
    fireEvent.touchEnd(viewport, { changedTouches: [{ clientX: 250, clientY: 100 }], touches: [] });
    expect(screen.getByRole("img", { name: "Page 3 of 6" })).toBeInTheDocument();
    // Zoomed in, a sideways drag scrolls the page instead of turning it.
    fireEvent.touchStart(viewport, { touches: [{ clientX: 200, clientY: 20 }] });
    fireEvent.touchEnd(viewport, { changedTouches: [{ clientX: 50, clientY: 25 }], touches: [] });
    expect(screen.getByRole("img", { name: "Page 3 of 6" })).toBeInTheDocument();
    fireEvent.keyDown(screen.getByLabelText("Reading page"), { key: "ArrowRight" });
    expect(screen.getByRole("img", { name: "Page 3 of 6" })).toBeInTheDocument();
  });

  it("rejects upstream image URLs throughout the gallery and reader", async () => {
    stubFetch(c => {
      if (c.url === "/v1/nclient/status") return { body: { data: { enabled: true } } };
      if (c.url.startsWith("/v1/nclient/search")) return { body: { data: { items: [{ ...summary, thumbUrl: "https://example.org/thumb.jpg" }], page: 1, numPages: 1 } } };
      if (c.url === "/v1/nclient/galleries/7") return { body: { data: { ...detail, coverUrl: "https://example.org/cover.jpg", related: [], pages: detail.pages.map(p => ({ ...p, url: "https://example.org/page.jpg", thumbUrl: "https://example.org/t.jpg" })) } } };
      return { body: { data: {} } };
    });
    renderNClient();
    fireEvent.click(await screen.findByRole("button", { name: /^Placeholder volume/ }));
    expect(await screen.findByRole("heading", { name: "Placeholder volume" })).toBeInTheDocument();
    expect(document.querySelectorAll(".nclient img")).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Resume" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Image could not be loaded");
    expect(document.querySelectorAll(".nclient img")).toHaveLength(0);
  });

  it("offers a fresh detail request when a signed image expires", async () => {
    const calls = serve(); renderNClient(); await openReader();
    fireEvent.error(screen.getByRole("img", { name: "Page 2 of 6" }));
    fireEvent.click(await screen.findByRole("button", { name: "Refresh" }));
    await screen.findByRole("button", { name: "Resume" });
    expect(calls.filter(c => c.url === "/v1/nclient/galleries/7")).toHaveLength(2);
  });

  it("reports list and progress failures", async () => {
    const options = { failSearch: true, failSave: true }; serve(options); renderNClient();
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not load NClient");
    options.failSearch = false; fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await openReader();
    expect(await screen.findByRole("alert")).toHaveTextContent("Progress could not be saved");
    options.failSave = false; fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Progress saved");
  });

  it("discards late gallery responses after returning to search", async () => {
    const late = deferred<{ body: unknown }>();
    stubFetch(c => c.url === "/v1/nclient/status" ? { body: { data: { enabled: true } } } : c.url === "/v1/nclient/random" ? late.promise : { body: { data: { items: [], page: 1, numPages: 1 } } });
    renderNClient(); fireEvent.click(await screen.findByRole("button", { name: "Random" }));
    fireEvent.click(screen.getByRole("button", { name: "Search" }));
    late.resolve({ body: { data: detail } });
    await waitFor(() => expect(screen.queryByRole("heading", { name: "Placeholder volume" })).toBeNull());
  });

  describe("Save to Lucina", () => {
    const running = (done: number) => ({ job: "job1", state: "running", total: 6, done, saved: Array.from({ length: done }, (_, index) => ({ index, itemId: `i${index}`, duplicate: false })), failed: [] });
    const finishedJob = (extra: Record<string, unknown> = {}) => ({ job: "job1", state: "done", total: 6, done: 6, saved: Array.from({ length: 6 }, (_, index) => ({ index, itemId: `i${index}`, duplicate: index < 2 })), failed: [], ...extra });
    const savePosts = (calls: Call[]) => calls.filter(c => c.url === "/v1/nclient/galleries/7/save");

    it("starts a gallery save, shows its progress and the result", async () => {
      const calls = serve({ saveStates: [running(2), running(4), finishedJob()] });
      renderNClient(); await openDetail();
      fireEvent.click(screen.getByRole("button", { name: "Save to Lucina" }));
      expect(await screen.findByText("Saving 2 of 6…")).toBeInTheDocument();
      expect(screen.getByRole("progressbar", { name: "Saving to Lucina" })).toHaveAttribute("value", "2");
      await screen.findByText(/Saved 6 pages to Lucina's Gallery \(2 already there\)/);
      expect(savePosts(calls)).toHaveLength(1);
      expect(savePosts(calls)[0]!.body).toEqual({ target: "gallery" });
      expect(screen.getByRole("button", { name: "Save to Lucina again" })).toBeInTheDocument();
      // No offer to put a comic in the Library: the Library reads Kindle formats only.
      expect(screen.queryByRole("button", { name: /library/i })).toBeNull();
    });

    it("saves from the reader's bar too, and the job survives closing the reader", async () => {
      const calls = serve({ saveStates: [running(1), running(3), finishedJob({ failed: [{ index: 4, error: "refused" }], saved: [] })] });
      renderNClient(); await openReader();
      const bar = document.querySelector(".nclient-reader .nclient-save") as HTMLElement;
      fireEvent.click(within(bar).getByRole("button", { name: "Save to Lucina" }));
      await within(bar).findByText(/Saving \d of 6…/);
      fireEvent.keyDown(window, { key: "Escape" });
      await screen.findByRole("button", { name: "Resume" });
      await screen.findByText(/Saved 0 pages to Lucina's Gallery \(1 failed\)/);
      expect(savePosts(calls)).toHaveLength(1);
    });

    it("can stop a running save", async () => {
      const calls = serve({ saveStates: [running(1)] });
      renderNClient(); await openDetail();
      fireEvent.click(screen.getByRole("button", { name: "Save to Lucina" }));
      fireEvent.click(await screen.findByRole("button", { name: "Stop saving" }));
      await screen.findByText("Stopped after 2 pages", {}, { timeout: 5000 });
      expect(calls.some(c => c.method === "DELETE" && c.url === "/v1/nclient/saves/job1")).toBe(true);
    });

    it.each([
      [404, "This gallery is not available."],
      [429, "Too many saves are waiting. Try again in a moment."],
      [500, "Could not save this gallery."],
    ])("reports a refused start (%s)", async (status, text) => {
      serve({ saveStart: { status, body: { ok: false, error: "x" } } });
      renderNClient(); await openDetail();
      fireEvent.click(screen.getByRole("button", { name: "Save to Lucina" }));
      expect(await screen.findByText(text)).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Save to Lucina again" })).toBeInTheDocument();
    });

    it("says when Lucina's Gallery is not reachable", async () => {
      serve({ saveStates: [{ job: "job1", state: "failed", total: 6, done: 3, saved: [], failed: [], error: "media_unavailable" }] });
      renderNClient(); await openDetail();
      fireEvent.click(screen.getByRole("button", { name: "Save to Lucina" }));
      expect(await screen.findByText("Lucina's Gallery is not reachable right now.")).toBeInTheDocument();
    });
  });
});
