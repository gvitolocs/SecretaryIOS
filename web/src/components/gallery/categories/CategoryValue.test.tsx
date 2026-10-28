import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../../../api/gallery";
import type { ItemCategory } from "../../../api/gallery";
import { setLocale } from "../../../i18n";
import { deferred } from "../../../test/fetch";
import { details, galleryItem } from "../../../test/galleryFixtures";
import { PhotoInfo, resetPhotoInfoCaches } from "../PhotoInfo";
import { CategoryValue } from "./CategoryValue";

vi.mock("../../../api/gallery", async (original) => ({
  ...(await original<typeof import("../../../api/gallery")>()),
  getDetails: vi.fn(),
  setItemCategory: vi.fn(),
  listAlbums: vi.fn(),
  listSources: vi.fn(),
}));
vi.mock("../../../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));

const { setItemCategory, getDetails } = vi.mocked(api);
const receipt: ItemCategory = { category: "receipt", confidence: 0.8, decidedBy: "rules", autoCategory: null };
const owned = (category: ItemCategory["category"], autoCategory: ItemCategory["category"] | null = "receipt"): ItemCategory => ({ category, confidence: 1, decidedBy: "owner", autoCategory });

beforeEach(() => {
  setLocale("en");
  resetPhotoInfoCaches();
  setItemCategory.mockReset();
  getDetails.mockReset();
  vi.mocked(api.listAlbums).mockResolvedValue([]);
  vi.mocked(api.listSources).mockResolvedValue({ sources: [], all: { photos: 0, videos: 0 } });
});
afterEach(() => vi.unstubAllGlobals());

const change = () => screen.getByRole("button", { name: "Change the category" });
const menu = () => screen.getByRole("list", { name: "Change the category" });

describe("the category of a photo", () => {
  it("names the category and says the server chose it", () => {
    render(() => <CategoryValue itemId="a" category={receipt} />);
    expect(screen.getByText("Receipt")).toBeInTheDocument();
    expect(screen.getByText("automatic")).toBeInTheDocument();
    expect(screen.queryByRole("list")).toBeNull();
  });

  it("says nothing more for a plain photo, and 'set by you' for the owner's own choice", () => {
    const { unmount } = render(() => <CategoryValue itemId="a" category={{ ...receipt, category: "photo" }} />);
    expect(screen.getByText("Photo")).toBeInTheDocument();
    expect(screen.queryByText("automatic")).toBeNull();
    unmount();
    render(() => <CategoryValue itemId="a" category={owned("photo")} />);
    expect(screen.getByText("set by you")).toBeInTheDocument();
  });

  it("offers 'Not a receipt' and the moves, and saves the owner's choice", async () => {
    setItemCategory.mockResolvedValue(owned("photo"));
    const changed = vi.fn();
    render(() => <CategoryValue itemId="a" category={receipt} onChanged={changed} />);
    fireEvent.click(change());
    expect(change()).toHaveAttribute("aria-expanded", "true");
    const labels = within(menu()).getAllByRole("button").map((b) => b.textContent);
    expect(labels).toEqual(["Not a receipt", "Move to Screenshots", "Move to Documents", "Move to Notes", "Move to Tickets", "Move to Maps", "Move to Memes", "Move to Food", "Move to Pets"]);
    fireEvent.click(screen.getByRole("button", { name: "Not a receipt" }));
    await waitFor(() => expect(setItemCategory).toHaveBeenCalledWith("a", "photo"));
    await waitFor(() => expect(screen.getByText("set by you")).toBeInTheDocument());
    expect(screen.getByText("Photo")).toBeInTheDocument();
    expect(screen.queryByRole("list")).toBeNull();
    expect(changed).toHaveBeenCalledWith(owned("photo"));
  });

  it("moves to another category", async () => {
    setItemCategory.mockResolvedValue(owned("document"));
    render(() => <CategoryValue itemId="a" category={receipt} />);
    fireEvent.click(change());
    fireEvent.click(screen.getByRole("button", { name: "Move to Documents" }));
    await waitFor(() => expect(setItemCategory).toHaveBeenCalledWith("a", "document"));
    await waitFor(() => expect(screen.getByText("Document")).toBeInTheDocument());
  });

  it("goes back to automatic from the owner's choice and shows what the server decides then", async () => {
    setItemCategory.mockResolvedValue(receipt);
    render(() => <CategoryValue itemId="a" category={owned("photo")} />);
    fireEvent.click(change());
    expect(within(menu()).queryByRole("button", { name: "Not a photo" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Back to automatic" }));
    await waitFor(() => expect(setItemCategory).toHaveBeenCalledWith("a", null));
    await waitFor(() => expect(screen.getByText("Receipt")).toBeInTheDocument());
    expect(screen.getByText("automatic")).toBeInTheDocument();
  });

  it("disappears when the server has no category for the photo any more", async () => {
    setItemCategory.mockResolvedValue(null);
    render(() => <CategoryValue itemId="a" category={owned("photo")} />);
    fireEvent.click(change());
    fireEvent.click(screen.getByRole("button", { name: "Back to automatic" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Change the category" })).toBeNull());
  });

  it("keeps the menu and says so when the change fails", async () => {
    setItemCategory.mockRejectedValue(new Error("boom"));
    render(() => <CategoryValue itemId="a" category={receipt} />);
    fireEvent.click(change());
    fireEvent.click(screen.getByRole("button", { name: "Not a receipt" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't change the category. Try again.");
    expect(screen.getByText("Receipt")).toBeInTheDocument();
    expect(menu()).toBeInTheDocument();
    expect(within(menu()).getAllByRole("button")[0]).not.toBeDisabled();
  });

  it("takes one choice at a time", async () => {
    const d = deferred<ItemCategory | null>();
    setItemCategory.mockReturnValue(d.promise);
    render(() => <CategoryValue itemId="a" category={receipt} />);
    fireEvent.click(change());
    fireEvent.click(screen.getByRole("button", { name: "Not a receipt" }));
    for (const b of within(menu()).getAllByRole("button")) expect(b).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Move to Maps" }));
    expect(setItemCategory).toHaveBeenCalledTimes(1);
    d.resolve(owned("photo"));
    await waitFor(() => expect(screen.getByText("set by you")).toBeInTheDocument());
  });

  it("ignores an answer that comes after the viewer moved to another photo", async () => {
    const d = deferred<ItemCategory | null>();
    setItemCategory.mockReturnValue(d.promise);
    const changed = vi.fn();
    const [id, setId] = createSignal("a");
    const [cat, setCat] = createSignal<ItemCategory>(receipt);
    render(() => <CategoryValue itemId={id()} category={cat()} onChanged={changed} />);
    fireEvent.click(change());
    fireEvent.click(screen.getByRole("button", { name: "Not a receipt" }));
    setId("b");
    setCat({ category: "pet", confidence: 0.9, decidedBy: "rules", autoCategory: null });
    d.resolve(owned("photo"));
    await waitFor(() => expect(screen.getByText("Pet")).toBeInTheDocument());
    expect(changed).not.toHaveBeenCalled();
    expect(screen.queryByText("set by you")).toBeNull();
  });
});

describe("the info panel", () => {
  const open = (category: ItemCategory | null) => {
    getDetails.mockImplementation(async (id) => details(galleryItem(id), { category }));
    render(() => <PhotoInfo item={galleryItem("a")} onClose={() => {}} onCaption={async () => {}} />);
  };

  it("shows the category row with the picker once the details are in", async () => {
    open(receipt);
    expect(await screen.findByText("Category")).toBeInTheDocument();
    expect(screen.getByText("Receipt")).toBeInTheDocument();
    setItemCategory.mockResolvedValue(owned("photo"));
    fireEvent.click(screen.getByRole("button", { name: "Change the category" }));
    fireEvent.click(screen.getByRole("button", { name: "Not a receipt" }));
    await waitFor(() => expect(setItemCategory).toHaveBeenCalledWith("a", "photo"));
    await waitFor(() => expect(screen.getByText("set by you")).toBeInTheDocument());
  });

  it("has no category row while the server has no category for the photo", async () => {
    open(null);
    await waitFor(() => expect(getDetails).toHaveBeenCalled());
    await screen.findByText("Albums");
    expect(screen.queryByText("Category")).toBeNull();
    expect(screen.queryByRole("button", { name: "Change the category" })).toBeNull();
  });
});
