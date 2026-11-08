import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { stubFetch, type Call } from "../test/fetch";
import { rawItem, sizeTheBox } from "../test/galleryGrid";
import { renderAt } from "../test/router";
import GalleryPetView from "./GalleryPet";
import GalleryPetsView from "./GalleryPets";

vi.mock("../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));
vi.mock("../components/gallery/browse/MemoriesStrip", () => ({ MemoriesStrip: () => null }));
vi.mock("../components/gallery/GalleryViewer", () => ({ GalleryViewer: () => null }));

const rawPet = (id: string, name: string | null, crops = 5) => ({
  id,
  name,
  hidden: false,
  birth_date: null,
  crop_count: crops,
  item_count: crops,
  cover_crop_id: `${id}_crop`,
  thumb_url: `/v1/media/pets/${id}/thumb?v=1`,
});

/** The pets routes as lucina-media answers them: pet-crops, pets, crop_count, `pet` on a crop. */
function petsBackend() {
  let pets = [rawPet("pt1", "Miso", 9), rawPet("pt2", null, 4)];
  let unassigned = [{ id: "pc9", item_id: "i9", thumb_url: "/v1/media/pet-crops/pc9/thumb", score: 0.8 }];
  const calls: Call[] = stubFetch((call) => {
    const url = new URL(call.url, "http://x");
    const key = `${call.method} ${url.pathname}`;
    if (key === "GET /v1/media/pet-crops/settings")
      return { body: { enabled: true, model: "yolox-s+dinov2-small", scanned: 10, pending: 0, crops: 13, pets: pets.length, running: false, paused_hot: false, paused_busy: false } };
    if (key === "GET /v1/media/pets") return { body: { pets } };
    if (key === "GET /v1/media/pet-crops/unassigned") return { body: { crops: unassigned, next_cursor: null } };
    if (key === "GET /v1/media/pets/pt1") return { body: pets[0] };
    if (key === "PATCH /v1/media/pets/pt2") {
      pets = pets.map((p) => (p.id === "pt2" ? { ...p, name: (call.body as { name: string }).name } : p));
      return { body: pets[1] };
    }
    if (key === "POST /v1/media/pets/pt1/merge") {
      pets = [{ ...pets[0]!, crop_count: 13 }];
      return { body: pets[0] };
    }
    if (key === "PUT /v1/media/pet-crops/pc9") {
      unassigned = [];
      return { body: { id: "pc9", box: { x: 0.1, y: 0.1, w: 0.3, h: 0.3 }, score: 0.8, source: "manual", pet: { id: "pt1", name: "Miso" } } };
    }
    if (url.pathname === "/v1/media/pets/pt1/items") return { body: { items: ["m1", "m2"].map((id, i) => rawItem(id, {}, i)), next_cursor: null } };
    if (url.pathname === "/v1/media/sources") return { body: { sources: [], all: { photos: 2, videos: 0 } } };
    if (url.pathname === "/v1/media/items/months") return { body: { months: [] } };
    return { status: 404, body: { error: { code: "not_found", message: `${key} not stubbed` } } };
  });
  return { calls };
}

beforeEach(() => {
  setLocale("en");
  sizeTheBox();
});
afterEach(() => vi.unstubAllGlobals());

describe("/gallery/pets", () => {
  it("lists the pets from the pets routes, renames, merges and links each to its own page", async () => {
    const { calls } = petsBackend();
    renderAt("/gallery/pets", () => <Route path="/gallery/pets" component={GalleryPetsView} />);
    const grid = await screen.findByRole("list", { name: "Pets" });
    const miso = within(grid).getByRole("button", { name: "Rename Miso" }).closest(".pp-tile") as HTMLElement;
    expect(miso.querySelector("img")?.getAttribute("src")).toBe("/v1/media/pets/pt1/thumb?v=1");
    expect(within(miso).getByRole("link")).toHaveAttribute("href", "/gallery/pet/pt1");
    expect(within(grid).getByRole("button", { name: "Rename Unnamed pet" })).toHaveTextContent("Add a name");
    // Nothing about faces or people was asked for.
    expect(calls.some((c) => c.url.includes("/faces") || c.url.includes("/people"))).toBe(false);

    fireEvent.click(within(grid).getByRole("button", { name: "Rename Unnamed pet" }));
    const field = await screen.findByLabelText("Name of this pet");
    fireEvent.input(field, { target: { value: "Pepe" } });
    fireEvent.submit(field.closest("form")!);
    await waitFor(() => expect(calls.find((c) => c.method === "PATCH")?.body).toEqual({ name: "Pepe" }));
    expect(await within(grid).findByRole("button", { name: "Rename Pepe" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Select" }));
    fireEvent.click(screen.getByRole("button", { name: "Select Miso" }));
    fireEvent.click(screen.getByRole("button", { name: "Select Pepe" }));
    fireEvent.click(screen.getByRole("button", { name: /Merge/ }));
    const dialog = await screen.findByRole("dialog", { name: "Merge pets" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Merge 2 pets" }));
    expect(await screen.findByText("Merged 2 pets into one.")).toBeInTheDocument();
    expect(calls.find((c) => c.method === "POST")?.url).toBe("/v1/media/pets/pt1/merge");
  });

  it("shows the animals without a pet from the pet-crops route", async () => {
    petsBackend();
    renderAt("/gallery/pets", () => <Route path="/gallery/pets" component={GalleryPetsView} />);
    expect(await screen.findByText("Animals without a pet")).toBeInTheDocument();
    await waitFor(() => expect(document.querySelector('img[src="/v1/media/pet-crops/pc9/thumb"]')).not.toBeNull());
  });
});

describe("/gallery/pet/:id", () => {
  it("shows the pet's name over the photos from its own route, with a way back to Pets", async () => {
    const { calls } = petsBackend();
    renderAt("/gallery/pet/pt1", () => <Route path="/gallery/pet/:id" component={GalleryPetView} />);
    expect(await screen.findByRole("heading", { name: "Miso" })).toBeInTheDocument();
    await waitFor(() => expect(document.querySelectorAll(".g-tile")).toHaveLength(2));
    expect(screen.getByRole("link", { name: "Pets" })).toHaveAttribute("href", "/gallery/pets");
    expect(screen.getByRole("searchbox")).toHaveAttribute("placeholder", expect.stringContaining("Miso"));
    expect(calls.some((c) => c.url.startsWith("/v1/media/pets/pt1/items"))).toBe(true);
    // Search by meaning knows people only: a pet's page never asks for it.
    expect(calls.some((c) => c.url.includes("/embeddings") || c.url.includes("/search"))).toBe(false);
  });
});
