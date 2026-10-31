import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { rawItem, sizeTheBox } from "../test/galleryGrid";
import { peopleBackend, rawPerson, type PeopleBackend } from "../test/peopleBackend";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import GalleryView from "./Gallery";
import GalleryPersonView from "./GalleryPerson";

vi.mock("../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));
vi.mock("../components/gallery/browse/MemoriesStrip", () => ({ MemoriesStrip: () => null }));
vi.mock("../components/gallery/GalleryViewer", () => ({
  GalleryViewer: (p: { items: { id: string }[]; index: number; onClose: () => void; onPerson?: (id: string) => void; onPeopleChanged?: () => void }) => (
    <div data-testid="viewer">
      <span>{p.items[p.index]?.id}</span>
      <button type="button" onClick={p.onClose}>
        close viewer
      </button>
      <button type="button" onClick={() => p.onPerson?.("p9")}>
        open other person
      </button>
      <button type="button" onClick={() => p.onPeopleChanged?.()}>
        people changed
      </button>
    </div>
  ),
}));

const IDS = ["a1", "a2", "a3", "a4", "a5"];
const ready = { enabled: true, model: "clip", indexed: 100, pending: 0, running: false, paused_hot: false };
const items = (ids: string[]) => ids.map((id, i) => rawItem(id, {}, i));

function setup(over: { smart?: boolean; person?: Partial<ReturnType<typeof rawPerson>> | null; ids?: string[]; pageSize?: number } = {}) {
  const searches: URLSearchParams[] = [];
  const words: URLSearchParams[] = [];
  const b = peopleBackend({
    people: over.person === null ? [] : [rawPerson("p1", { name: "Alma", item_count: IDS.length, ...over.person })],
    items: new Map([["p1", items(over.ids ?? IDS)]]),
    pageSize: over.pageSize ?? 3,
    extra: (_call, url) => {
      if (url.pathname === "/v1/media/sources") return { body: { sources: [], all: { photos: 5, videos: 0 } } };
      if (url.pathname === "/v1/media/items/months") return { body: { months: [] } };
      if (url.pathname === "/v1/media/embeddings/status") return over.smart ? { body: ready } : { status: 404, body: { error: { code: "x", message: "x" } } };
      if (url.pathname === "/v1/media/items/search") {
        searches.push(url.searchParams);
        return { body: { items: items(["a4", "a2"]), scores: [0.9, 0.8], next_cursor: null, status: ready } };
      }
      if (url.pathname === "/v1/media/people/p1/items" && url.searchParams.get("q")) {
        words.push(url.searchParams);
        return { body: { items: items(["a3"]), next_cursor: null } };
      }
      return undefined;
    },
  });
  return { b, searches, words };
}

function mount(path = "/gallery/person/p1") {
  return renderAt(path, () => (
    <>
      <Route path="/gallery/person/:id" component={GalleryPersonView} />
      <Route path="/gallery/people" component={() => <p>people page</p>} />
      <Route path="/gallery/person/p9" component={() => <p>other person page</p>} />
      <Route path="/gallery" component={() => <p>gallery page</p>} />
    </>
  ));
}

const shownIds = () =>
  [...document.querySelectorAll<HTMLElement>(".g-tile")]
    .sort((a, b) => Number(a.dataset.index) - Number(b.dataset.index))
    .map((t) => /items\/([^/]+)\/thumb/.exec(t.querySelector("img")?.getAttribute("src") ?? "")?.[1]);
const listCalls = (b: PeopleBackend) => b.calls.filter((c) => /^\/v1\/media\/people\/p1\/items/.test(c.url));
const searchBox = () => screen.getByRole("searchbox");

beforeEach(() => {
  setLocale("en");
  sizeTheBox();
  try {
    localStorage.clear();
  } catch {
    // ignore
  }
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("a person's page (/gallery/person/:id)", () => {
  it("shows the person's name, face and photo count, over the Gallery's grid of their photos", async () => {
    const { b } = setup();
    mount();
    expect(await screen.findByRole("heading", { name: "Alma" })).toBeInTheDocument();
    await waitFor(() => expect(shownIds()).toEqual(["a1", "a2", "a3", "a4", "a5"]));
    expect(screen.getByText("5 photos")).toBeInTheDocument();
    expect(document.querySelector(".pp-person img")?.getAttribute("src")).toBe("/v1/media/people/p1/thumb?v=1");
    // The photos come from the person's own route, never from the plain listing, with no month scrubber, source bar or memories.
    expect(b.calls.some((c) => c.method === "GET" && /^\/v1\/media\/items\?/.test(c.url))).toBe(false);
    expect(b.calls.some((c) => c.url.startsWith("/v1/media/items/months"))).toBe(false);
    expect(document.querySelector(".g-source")).toBeNull();
    expect(screen.getByRole("link", { name: "People" })).toHaveAttribute("href", "/gallery/people");
  });

  it("pages with the cursor as the grid asks for more", async () => {
    const { b } = setup({ pageSize: 3 });
    mount();
    await waitFor(() => expect(shownIds()).toHaveLength(5));
    const urls = listCalls(b).map((c) => new URL(c.url, "http://x").searchParams.get("cursor"));
    expect(urls).toEqual([null, "3"]);
    expect(new URL(listCalls(b)[0]!.url, "http://x").searchParams.get("person")).toBeNull();
  });

  it("groups the photos by day like the main grid", async () => {
    setup();
    mount();
    await waitFor(() => expect(shownIds()).toHaveLength(5));
    expect(document.querySelectorAll(".g-head").length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Months" })).toBeInTheDocument();
  });

  it("opens a photo in the viewer, and the viewer can move to another person", async () => {
    setup();
    mount();
    await waitFor(() => expect(shownIds()).toHaveLength(5));
    fireEvent.click(document.querySelector<HTMLElement>('.g-tile[data-index="1"] .g-open')!);
    expect(await screen.findByTestId("viewer")).toHaveTextContent("a2");
    fireEvent.click(screen.getByRole("button", { name: "open other person" }));
    expect(await screen.findByText("other person page")).toBeInTheDocument();
    expect(screen.queryByTestId("viewer")).toBeNull();
  });

  it("refreshes the photo count when the viewer changes who is in a photo", async () => {
    const { b } = setup();
    mount();
    await waitFor(() => expect(shownIds()).toHaveLength(5));
    fireEvent.click(document.querySelector<HTMLElement>('.g-tile[data-index="0"] .g-open')!);
    await screen.findByTestId("viewer");
    b.people[0]!.item_count = 4;
    fireEvent.click(screen.getByRole("button", { name: "people changed" }));
    expect(await screen.findByText("4 photos")).toBeInTheDocument();
  });

  it("says when the person has no photos", async () => {
    setup({ ids: [], person: { item_count: 0 } });
    mount();
    expect(await screen.findByText("No photos of this person.")).toBeInTheDocument();
  });

  it("says so for a person that is gone", async () => {
    setup({ person: null, ids: [] });
    mount();
    expect(await screen.findByText("This person doesn't exist any more.")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "People" })).toBeInTheDocument();
  });

  it("renames the person from the header", async () => {
    const { b } = setup();
    mount();
    await screen.findByRole("heading", { name: "Alma" });
    fireEvent.click(await screen.findByRole("button", { name: "Rename Alma" }));
    const input = await screen.findByLabelText("Name of this person");
    fireEvent.input(input, { target: { value: "Alma Rossi" } });
    fireEvent.submit(input.closest("form")!);
    expect(await screen.findByRole("heading", { name: "Alma Rossi" })).toBeInTheDocument();
    expect(b.calls.find((c) => c.method === "PATCH")).toMatchObject({ url: "/v1/media/people/p1", body: { name: "Alma Rossi" } });
  });

  it("saves, clears and refuses a birth date", async () => {
    const { b } = setup();
    mount();
    const date = await screen.findByLabelText("Birth date");
    fireEvent.change(date, { target: { value: "1990-04-12" } });
    await waitFor(() => expect(b.calls.find((c) => c.method === "PATCH")?.body).toEqual({ birth_date: "1990-04-12" }));
    const clear = await screen.findByRole("button", { name: "Remove" });
    fireEvent.click(clear);
    await waitFor(() => expect(b.calls.filter((c) => c.method === "PATCH").pop()?.body).toEqual({ birth_date: null }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Remove" })).toBeNull());
    // A day in the future is not a birth date: nothing is sent.
    const sent = b.calls.filter((c) => c.method === "PATCH").length;
    fireEvent.change(screen.getByLabelText("Birth date"), { target: { value: "2999-01-01" } });
    expect(await screen.findByText("That isn't a valid date.")).toBeInTheDocument();
    expect(b.calls.filter((c) => c.method === "PATCH")).toHaveLength(sent);
  });
});

describe("search within a person", () => {
  it("says in the box whose photos it searches", async () => {
    setup();
    mount();
    expect(await screen.findByPlaceholderText("Search photos of Alma")).toBeInTheDocument();
  });

  it("asks the words search on the person's route, so only their photos match", async () => {
    const { words } = setup({ smart: false });
    mount();
    await waitFor(() => expect(shownIds()).toHaveLength(5));
    fireEvent.input(searchBox(), { target: { value: "beach" } });
    fireEvent.keyDown(searchBox(), { key: "Enter" });
    await waitFor(() => expect(shownIds()).toEqual(["a3"]));
    expect(words[0]!.get("q")).toBe("beach");
  });

  it("asks smart search with person= when it is on", async () => {
    const { searches, b } = setup({ smart: true });
    mount();
    await waitFor(() => expect(shownIds()).toHaveLength(5));
    await waitFor(() => expect(b.calls.some((c) => c.url.includes("embeddings/status"))).toBe(true));
    await new Promise((r) => setTimeout(r, 20));
    fireEvent.input(searchBox(), { target: { value: "dog" } });
    fireEvent.keyDown(searchBox(), { key: "Enter" });
    await waitFor(() => expect(shownIds()).toEqual(["a4", "a2"]));
    expect(searches[0]!.get("person")).toBe("p1");
    expect(searches[0]!.get("q")).toBe("dog");
    expect(searches[0]!.get("smart")).toBeNull();
  });

  it("goes back to all of the person's photos when the filters are cleared", async () => {
    setup({ smart: false });
    mount();
    await waitFor(() => expect(shownIds()).toHaveLength(5));
    fireEvent.input(searchBox(), { target: { value: "beach" } });
    fireEvent.keyDown(searchBox(), { key: "Enter" });
    await waitFor(() => expect(shownIds()).toEqual(["a3"]));
    fireEvent.click(within(document.querySelector(".g-filters")!).getByRole("button", { name: "Clear filters" }));
    await waitFor(() => expect(shownIds()).toHaveLength(5));
  });
});

describe("the Gallery's header", () => {
  it("links to the People page next to Albums and Places, and the main grid does not ask for people", async () => {
    const b = peopleBackend({
      extra: (_call, url) => {
        if (url.pathname === "/v1/media/sources") return { body: { sources: [], all: { photos: 1, videos: 0 } } };
        if (url.pathname === "/v1/media/items/months") return { body: { months: [] } };
        if (url.pathname === "/v1/media/embeddings/status") return { status: 404, body: { error: { code: "x", message: "x" } } };
        if (url.pathname === "/v1/media/items") return { body: { items: items(["m1"]), next_cursor: null } };
        return undefined;
      },
    });
    renderAt("/gallery", () => <Route path="/gallery" component={() => <GalleryView />} />);
    const link = await screen.findByRole("link", { name: "People" });
    expect(link).toHaveAttribute("href", "/gallery/people");
    const header = link.closest("header")!;
    const labels = [...header.querySelectorAll("a.g-iconbtn")].map((a) => a.getAttribute("aria-label"));
    expect(labels.indexOf("People")).toBe(labels.indexOf("Albums") + 1);
    expect(labels.indexOf("Places")).toBe(labels.indexOf("People") + 1);
    await waitFor(() => expect(shownIds()).toEqual(["m1"]));
    expect(b.calls.some((c) => c.url.includes("/people") || c.url.includes("/faces"))).toBe(false);
  });
});

describe("a person's page on the page system (desktop)", () => {
  it("keeps the Gallery header (People current), a back link to People and the person's name as the heading", async () => {
    stubWide();
    setup();
    mount();
    expect(await screen.findByRole("heading", { level: 1, name: "Gallery" })).toBeInTheDocument();
    const nav = screen.getByRole("navigation", { name: "Gallery sections" });
    expect(within(nav).getByRole("link", { name: "People" })).toHaveAttribute("aria-current", "page");
    expect(await screen.findByRole("heading", { level: 2, name: "Alma" })).toHaveClass("g-detail-title");
    expect(document.querySelector("a.pg-back")).toHaveAttribute("href", "/gallery/people");
    expect(document.querySelector(".g-bar")).toBeNull();
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(0);
    await waitFor(() => expect(shownIds()).toEqual(["a1", "a2", "a3", "a4", "a5"]));
    // The person's face, count and birth date stay above the full photo grid.
    expect(document.querySelector(".pp-person")).not.toBeNull();
    expect(document.querySelector(".pg-grid--full > .pg-main > .g-main .g-scroll")).not.toBeNull();
  });

  it("shows the friendly empty state when the person has no photos", async () => {
    stubWide();
    setup({ ids: [] });
    mount();
    await waitFor(() => expect(document.querySelector(".pg-empty")).not.toBeNull());
  });
});
