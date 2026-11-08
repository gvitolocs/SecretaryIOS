import { stubWide } from "../test/wide";
import { Route, useParams } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { rawItem } from "../test/galleryGrid";
import { type Call, stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import GalleryAlbumsView from "./GalleryAlbums";

vi.mock("../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));

const album = (id: string, count: number, extra: Record<string, unknown> = {}) => ({
  id,
  title: `Album ${id}`,
  system_key: null,
  cover_item: null,
  count,
  created_at: "2026-10-01T10:00:00Z",
  updated_at: "2026-10-02T10:00:00Z",
  ...extra,
});

interface Setup {
  scoped?: unknown[];
  plain?: unknown[];
  failList?: boolean;
}

function backend(setup: Setup = {}): Call[] {
  return stubFetch((call) => {
    const url = new URL(call.url, "http://x");
    if (url.pathname === "/v1/media/albums" && call.method === "GET") {
      if (setup.failList) return { status: 500, body: { error: { code: "boom", message: "boom" } } };
      return { body: { albums: url.searchParams.get("scope") === "gallery" ? (setup.scoped ?? []) : (setup.plain ?? setup.scoped ?? []) } };
    }
    if (url.pathname === "/v1/media/albums" && call.method === "POST") return { status: 201, body: album("alb_new", 0, { title: (call.body as { title: string }).title }) };
    if (url.pathname === "/v1/media/items/p9") return { body: rawItem("p9", { thumbhash: "3OcRJYB4d3h/iIeHeEh3eIhw+j3A" }) };
    if (url.pathname === "/v1/media/items" && call.method === "GET") return { body: { items: [rawItem("newest")], next_cursor: null } };
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
}

function mount() {
  return renderAt("/gallery/albums", () => (
    <>
      <Route path="/gallery/albums" component={GalleryAlbumsView} />
      <Route path="/gallery/album/:id" component={() => <p>album page {useParams().id}</p>} />
      <Route path="/gallery" component={() => <p>gallery page</p>} />
    </>
  ));
}

beforeEach(() => setLocale("en"));
afterEach(() => vi.unstubAllGlobals());

describe("the albums page", () => {
  it("shows the owner's albums as cards with their counts, then the built-in ones, and never a share album", async () => {
    backend({
      scoped: [
        album("a1", 3, { cover_item: "p9" }),
        album("a2", 1, { updated_at: "2026-09-01T10:00:00Z" }),
        album("lucina", 5, { title: "Lucina", system_key: "lucina" }),
        album("tok", 2, { title: "Shared photos", system_key: "share:tok1" }),
      ],
    });
    mount();
    const own = await screen.findByRole("list", { name: "Albums" });
    const cards = within(own).getAllByRole("link");
    expect(cards.map((c) => c.textContent)).toEqual(["Album a13 photos", "Album a21 photo"]);
    expect(cards[0]).toHaveAttribute("href", "/gallery/album/a1");
    const builtIn = screen.getByRole("list", { name: "Made by Lucina" });
    expect(within(builtIn).getByRole("link")).toHaveTextContent("Lucina5 photos");
    expect(screen.queryByText("Shared photos")).toBeNull();
    // The built-in section comes after the owner's albums.
    expect(own.compareDocumentPosition(builtIn) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("covers: the chosen cover item, else the newest photo of the album, else the album mark", async () => {
    const calls = backend({ scoped: [album("a1", 3, { cover_item: "p9" }), album("a2", 2)], plain: [album("a1", 3), album("a2", 2), album("a3", 0)] });
    mount();
    await waitFor(() => expect(document.querySelectorAll(".al-cover__img")).toHaveLength(2));
    const images = [...document.querySelectorAll<HTMLImageElement>(".al-cover__img")].map((i) => i.getAttribute("src"));
    expect(images).toEqual(["/v1/media/items/p9/thumb?size=256", "/v1/media/items/newest/thumb?size=256"]);
    const lists = calls.filter((c) => c.url.startsWith("/v1/media/items?")).map((c) => new URLSearchParams(c.url.split("?")[1]));
    expect(lists).toHaveLength(1);
    expect(lists[0]!.get("album")).toBe("a2");
    expect(lists[0]!.get("limit")).toBe("1");
    // The empty album is listed (from the plain listing) with the mark, and without any request for a cover.
    const empty = screen.getByRole("link", { name: "Open the album Album a3" });
    expect(empty.querySelector("img")).toBeNull();
    expect(empty).toHaveTextContent("0 photos");
  });

  it("says so when there are no albums", async () => {
    backend();
    mount();
    expect(await screen.findByText(/No albums yet\./)).toBeInTheDocument();
  });

  it("shows an error with a retry", async () => {
    backend({ failList: true });
    mount();
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't load the albums.");
    expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
  });

  it("makes an album with New album and opens it", async () => {
    const calls = backend({ scoped: [album("a1", 1)] });
    mount();
    await screen.findByRole("list", { name: "Albums" });
    fireEvent.click(screen.getByRole("button", { name: /New album/ }));
    const dialog = await screen.findByRole("dialog");
    const input = within(dialog).getByLabelText("Name");
    fireEvent.input(input, { target: { value: "  Spring  " } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Create" }));
    expect(await screen.findByText("album page alb_new")).toBeInTheDocument();
    const post = calls.find((c) => c.method === "POST")!;
    expect(post.body).toEqual({ title: "Spring" });
  });

  it("does not create an album without a name", async () => {
    const calls = backend({ scoped: [album("a1", 1)] });
    mount();
    await screen.findByRole("list", { name: "Albums" });
    fireEvent.click(screen.getByRole("button", { name: /New album/ }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByRole("button", { name: "Create" })).toBeDisabled();
    expect(calls.some((c) => c.method === "POST")).toBe(false);
  });
});

it("uses the shared desktop header and an album surface, including the empty state", async () => {
  stubWide(); backend(); mount();
  expect(await screen.findByRole("heading", { level: 1, name: "Gallery" })).toBeInTheDocument();
  const nav = screen.getByRole("navigation", { name: "Gallery sections" });
  expect(within(nav).getByRole("link", { name: "Albums" })).toHaveAttribute("aria-current", "page");
  expect(document.querySelectorAll(".pg-cta")).toHaveLength(1);
  expect(document.querySelector(".al-head")).toBeNull();
  expect(screen.getByRole("region", { name: "Albums" })).toHaveClass("pg-card");
  await waitFor(() => expect(document.querySelector(".pg-empty")).toHaveTextContent("No albums yet."));
});
