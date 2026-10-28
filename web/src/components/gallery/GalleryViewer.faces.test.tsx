import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../../api/gallery";
import type { GalleryItem } from "../../api/gallery";
import { setLocale } from "../../i18n";
import { setImageCoreForTests } from "../../lib/imageCore";
import { details, galleryItem } from "../../test/galleryFixtures";
import { type Call, stubFetch } from "../../test/fetch";
import { resetFacesAvailable } from "./faces/facesAvailable";
import { GalleryViewer } from "./GalleryViewer";
import { PhotoInfo, resetPhotoInfoCaches } from "./PhotoInfo";

vi.mock("../../api/gallery", async (original) => ({
  ...(await original<typeof import("../../api/gallery")>()),
  getDetails: vi.fn(),
  patchItem: vi.fn(),
  batch: vi.fn(),
  listAlbums: vi.fn(),
  listSources: vi.fn(),
}));
vi.mock("../../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));

const { getDetails } = vi.mocked(api);

beforeAll(() => setImageCoreForTests(null));

function server(enabled: boolean): Call[] {
  return stubFetch((call) => {
    const url = new URL(call.url, "http://x");
    if (url.pathname === "/v1/media/faces/settings") return { body: { enabled } };
    if (/\/faces$/.test(url.pathname)) return { body: { faces: [] } };
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
}

function open(items: GalleryItem[] = [galleryItem("a")], extra: { onPerson?: (id: string) => void } = {}) {
  const [index, setIndex] = createSignal(0);
  render(() => <GalleryViewer items={items} index={index()} onIndex={setIndex} onClose={() => {}} onChanged={() => {}} onRemoved={() => {}} onPerson={extra.onPerson} />);
}
const key = (k: string, target: Element | Window = window) => fireEvent.keyDown(target, { key: k });
const toolbar = () => screen.getByRole("toolbar", { name: "Photo actions" });
const facesButton = () => within(toolbar()).queryByRole("button", { name: "Faces" });
const faceRequests = (calls: Call[]) => calls.filter((c) => /\/faces$/.test(c.url));

beforeEach(() => {
  setLocale("en");
  resetPhotoInfoCaches();
  resetFacesAvailable();
  getDetails.mockReset();
  getDetails.mockImplementation(async (id) => details(galleryItem(id)));
  vi.mocked(api.listAlbums).mockResolvedValue([]);
  vi.mocked(api.listSources).mockResolvedValue({ sources: [], all: { photos: 0, videos: 0 } });
  try {
    localStorage.clear();
  } catch {
    // ignore
  }
});
afterEach(() => vi.unstubAllGlobals());

describe("the Faces toggle in the viewer's toolbar", () => {
  it("is there for a photo when recognition is on, with the key in its title", async () => {
    server(true);
    open();
    const button = await waitFor(() => {
      const found = facesButton();
      expect(found).not.toBeNull();
      return found!;
    });
    expect(button).toHaveAttribute("aria-pressed", "false");
    expect(button).toHaveAttribute("title", "Show faces (P)");
  });

  it("is not there when recognition is off, nor for a video", async () => {
    const calls = server(false);
    open();
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/media/faces/settings")).toBe(true));
    await new Promise((r) => setTimeout(r, 10));
    expect(facesButton()).toBeNull();
    // P does nothing then.
    key("p");
    expect(faceRequests(calls)).toEqual([]);
  });

  it("is not there for a video even when recognition is on", async () => {
    const calls = server(true);
    open([galleryItem("v", { kind: "video", mime: "video/mp4" })]);
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/media/faces/settings")).toBe(true));
    await new Promise((r) => setTimeout(r, 10));
    expect(facesButton()).toBeNull();
    key("p");
    expect(faceRequests(calls)).toEqual([]);
  });

  it("is toggled by the P key, and the faces of the photo are asked for when it turns on", async () => {
    const calls = server(true);
    open();
    await waitFor(() => expect(facesButton()).not.toBeNull());
    expect(faceRequests(calls)).toEqual([]);
    key("p");
    expect(facesButton()).toHaveAttribute("aria-pressed", "true");
    await waitFor(() => expect(faceRequests(calls).map((c) => c.url)).toEqual(["/v1/media/items/a/faces"]));
    key("P");
    expect(facesButton()).toHaveAttribute("aria-pressed", "false");
  });

  it("leaves the P key to a text field", async () => {
    server(true);
    open();
    await waitFor(() => expect(facesButton()).not.toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "Info" }));
    const caption = await screen.findByLabelText("Caption");
    key("p", caption);
    expect(facesButton()).toHaveAttribute("aria-pressed", "false");
  });
});

describe("the People row in the info panel", () => {
  async function panelWith(people: api.PersonRef[], onPerson?: (id: string) => void) {
    server(false);
    getDetails.mockResolvedValue(details(galleryItem("a"), { people }));
    open([galleryItem("a")], { onPerson });
    fireEvent.click(screen.getByRole("button", { name: "Info" }));
    return screen.getByRole("region", { name: "Info" });
  }

  it("lists the people as chips linking to their page, naming the unnamed ones", async () => {
    const panel = await panelWith([{ id: "p1", name: "Alma" }, { id: "p2", name: null }]);
    expect(await within(panel).findByRole("link", { name: "Alma" })).toHaveAttribute("href", "/gallery/person/p1");
    expect(within(panel).getByRole("link", { name: "Unnamed person" })).toHaveAttribute("href", "/gallery/person/p2");
  });

  it("opens the person through onPerson instead of a page load, but leaves a modified click to the browser", async () => {
    const onPerson = vi.fn();
    const panel = await panelWith([{ id: "p1", name: "Alma" }], onPerson);
    const link = await within(panel).findByRole("link", { name: "Alma" });
    // Whether the page's own handler took the click (default prevented), seen after it ran; the test then stops jsdom navigating.
    const taken: boolean[] = [];
    const watch = (e: Event) => {
      taken.push(e.defaultPrevented);
      e.preventDefault();
    };
    document.addEventListener("click", watch);
    fireEvent.click(link);
    expect(onPerson).toHaveBeenCalledWith("p1");
    onPerson.mockClear();
    fireEvent.click(link, { ctrlKey: true });
    document.removeEventListener("click", watch);
    expect(onPerson).not.toHaveBeenCalled();
    expect(taken).toEqual([true, false]);
  });

  it("shows a name from a server without the people API as plain text", async () => {
    const panel = await panelWith([{ id: null, name: "Bob" }]);
    expect(await within(panel).findByText("Bob")).toBeInTheDocument();
    expect(within(panel).queryByRole("link", { name: "Bob" })).toBeNull();
  });

  it("has no People row for a photo without people", async () => {
    const panel = await panelWith([]);
    await within(panel).findByText("Not in any album");
    expect(within(panel).queryByText("People")).toBeNull();
  });
});

describe("the people of a photo after a face changed hands", () => {
  it("asks for the details again when the people key changes, without blinking, and never shows the last photo's people", async () => {
    let people: api.PersonRef[] = [{ id: "p1", name: "Alma" }];
    getDetails.mockImplementation(async (id) => details(galleryItem(id), { people }));
    const [key, setKey] = createSignal(0);
    const [item, setItem] = createSignal(galleryItem("a"));
    render(() => <PhotoInfo item={item()} onClose={() => {}} onCaption={async () => {}} peopleKey={key()} />);
    expect(await screen.findByText("Alma")).toBeInTheDocument();
    expect(getDetails).toHaveBeenCalledTimes(1);

    // Another change to the same item (a favorite, a caption) asks for nothing.
    setItem(galleryItem("a", { favorite: true }));
    await new Promise((r) => setTimeout(r, 10));
    expect(getDetails).toHaveBeenCalledTimes(1);

    people = [{ id: "p1", name: "Alma" }, { id: "p2", name: "Bruno" }];
    setKey(1);
    expect(screen.getByText("Alma")).toBeInTheDocument();
    expect(await screen.findByText("Bruno")).toBeInTheDocument();
    expect(getDetails).toHaveBeenCalledTimes(2);
    expect(getDetails).toHaveBeenLastCalledWith("a");

    // The next photo's chips replace these only when its own details arrive.
    people = [{ id: "p3", name: "Chiara" }];
    setItem(galleryItem("b"));
    await waitFor(() => expect(screen.getByText("Chiara")).toBeInTheDocument());
    expect(screen.queryByText("Alma")).toBeNull();
  });
});
