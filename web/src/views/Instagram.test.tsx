import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { IgItem, IgPage } from "../api/instagram";
import { stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
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

const bundle = (over: { hasStories?: boolean; isPrivate?: boolean; highlights?: boolean; next?: string | null } = {}) => ({
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
  highlights: over.highlights ? [{ id: "highlight:1", title: "Best of", coverUrl: "/v1/instagram/s/9/x/h", mediaCount: 2 }] : [],
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
    if (url.startsWith("/v1/instagram/resolve")) return { body: { ok: true, data: r.resolve } };
    if (url === "/v1/instagram/recent") return { body: { ok: true, data: { items: r.recent ?? [] } } };
    if (url === "/v1/instagram/save") return { body: { ok: true, data: r.save ?? { saved: [{ index: 0, itemId: "itm_1", duplicate: false }], failed: [] } } };
    return { status: 404, body: {} };
  });
}

function renderIg(path = "/instagram") {
  return renderAt(path, () => <Route path="/instagram/:username?" component={InstagramView} />);
}

describe("Instagram", () => {
  afterEach(() => vi.restoreAllMocks());

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

  it("shows the profile header, counts and the four tabs", async () => {
    serve({ profile: bundle() });
    renderIg("/instagram/natgeo");
    expect(await screen.findByRole("heading", { name: /National Geographic/ })).toBeInTheDocument();
    expect(screen.getByRole("img", { name: "Verified" })).toBeInTheDocument();
    expect(screen.getByText("268M")).toBeInTheDocument();
    expect(screen.getByText("32K")).toBeInTheDocument();
    expect(screen.getByText("Step into wonder")).toBeInTheDocument();
    expect(screen.getAllByRole("tab").map((t) => t.textContent)).toEqual(["Stories", "Highlights", "Posts", "Reels"]);
    expect(screen.getByRole("tab", { name: "Posts" })).toHaveAttribute("aria-selected", "true");
    expect(within(screen.getByRole("tabpanel")).getAllByRole("button")).toHaveLength(4); // 3 tiles + Load more
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

  it("opens a highlight in the viewer", async () => {
    serve({ profile: bundle({ highlights: true }), highlightItems: { items: [item("h1"), item("h2")], nextCursor: null } });
    renderIg("/instagram/natgeo");
    fireEvent.click(await screen.findByRole("tab", { name: "Highlights" }));
    fireEvent.click(screen.getByRole("button", { name: /Best of/ }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("1 of 2")).toBeInTheDocument();
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
    expect(within(dialog).getByText("@natgeo")).toBeInTheDocument();
    expect(calls.some((c) => c.url.startsWith("/v1/instagram/resolve?url="))).toBe(true);
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
      expect(within(dialog).getByText("1 of 3")).toBeInTheDocument();
      expect(within(dialog).getByText("caption p1")).toBeInTheDocument();
      fireEvent.keyDown(window, { key: "ArrowRight" });
      expect(within(dialog).getByText("2 of 3")).toBeInTheDocument();
      expect(within(dialog).getByText("caption p2")).toBeInTheDocument();
      fireEvent.keyDown(window, { key: "ArrowLeft" });
      fireEvent.keyDown(window, { key: "ArrowLeft" });
      expect(within(dialog).getByText("1 of 3")).toBeInTheDocument();
      fireEvent.keyDown(window, { key: "Escape" });
      expect(screen.queryByRole("dialog")).toBeNull();
    });

    it("swipes", async () => {
      const { dialog } = await openFirst();
      fireEvent.touchStart(dialog, { touches: [{ clientX: 300, clientY: 400 }] });
      fireEvent.touchEnd(dialog, { changedTouches: [{ clientX: 120, clientY: 410 }] });
      expect(within(dialog).getByText("2 of 3")).toBeInTheDocument();
      fireEvent.touchStart(dialog, { touches: [{ clientX: 100, clientY: 400 }] });
      fireEvent.touchEnd(dialog, { changedTouches: [{ clientX: 260, clientY: 380 }] });
      expect(within(dialog).getByText("1 of 3")).toBeInTheDocument();
      // A mostly vertical drag is not a swipe.
      fireEvent.touchStart(dialog, { touches: [{ clientX: 100, clientY: 100 }] });
      fireEvent.touchEnd(dialog, { changedTouches: [{ clientX: 85, clientY: 20 }] });
      expect(within(dialog).getByText("1 of 3")).toBeInTheDocument();
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
      expect(calls).toBeTruthy();
    });

    it("saves the current slide and tells the owner", async () => {
      const { calls, dialog } = await openFirst();
      fireEvent.click(within(dialog).getByRole("button", { name: "Save to Lucina" }));
      expect(await within(dialog).findByText("Saved to Lucina", { selector: ".ig-toast" })).toBeInTheDocument();
      const save = calls.find((c) => c.url === "/v1/instagram/save")!;
      expect(save.method).toBe("POST");
      expect(save.headers["x-secretary-csrf"]).toBe("1");
      expect(save.body).toEqual({ items: [{ url: "/v1/instagram/s/9/x/p1", kind: "image", username: "natgeo", takenAt: 1_700_000_000, caption: "caption p1" }] });
    });

    it("says it is a duplicate", async () => {
      const { dialog } = await openFirst({ save: { saved: [{ index: 0, itemId: "itm_1", duplicate: true }], failed: [] } });
      fireEvent.click(within(dialog).getByRole("button", { name: "Save to Lucina" }));
      expect(await within(dialog).findByText("Already in Lucina", { selector: ".ig-toast" })).toBeInTheDocument();
    });

    it("reports a failed save and lets the owner try again", async () => {
      const { dialog } = await openFirst({ save: { saved: [], failed: [{ index: 0, error: "expired" }] } });
      fireEvent.click(within(dialog).getByRole("button", { name: "Save to Lucina" }));
      expect(await within(dialog).findByText(/Couldn't save: the link expired/)).toBeInTheDocument();
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
      expect(within(dialog).getByText("1 of 2")).toBeInTheDocument();
      fireEvent.click(within(dialog).getByRole("button", { name: "Save all 2" }));
      expect(await within(dialog).findByText("Saved 2 to Lucina", { selector: ".ig-toast" })).toBeInTheDocument();
      const body = calls.find((c) => c.url === "/v1/instagram/save")!.body as { items: { url: string; kind: string }[] };
      expect(body.items.map((i) => [i.url, i.kind])).toEqual([["/v1/instagram/s/9/x/c0", "image"], ["/v1/instagram/s/9/x/c1", "video"]]);
    });
  });
});
