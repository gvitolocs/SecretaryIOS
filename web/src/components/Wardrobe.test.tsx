import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { setLocale } from "../i18n";
import { stubFetch, type Call } from "../test/fetch";
import { merge, settings, WARDROBE } from "../test/wardrobe";
import { avatarSettings, closeWardrobe, loadAvatar, openWardrobe, resetWardrobeState, setAvatarPath, wardrobeOpen } from "../state/wardrobe";
import Wardrobe, { COM3D2_KEY, COM3D2_URL } from "./Wardrobe";

function serve(wardrobe: unknown = WARDROBE): Call[] {
  let current = settings();
  return stubFetch((c) => {
    if (c.url === "/v1/avatar/wardrobe") return { body: wardrobe };
    if (c.url === "/v1/avatar" && c.method === "GET") return { body: current };
    if (c.url === "/v1/avatar" && c.method === "PUT") {
      current = merge(current, c.body as Record<string, unknown>);
      return { body: current };
    }
    return { status: 404 };
  });
}

const puts = (calls: Call[]) => calls.filter((c) => c.method === "PUT").map((c) => c.body);
const tab = (name: string) => within(screen.getByRole("tablist", { name: "Wardrobe" })).getByRole("tab", { name });
const grid = () => screen.getByRole("tabpanel");

async function show(wardrobe?: unknown, title = "Wardrobe") {
  const calls = serve(wardrobe);
  await loadAvatar();
  render(() => <Wardrobe />);
  openWardrobe();
  await screen.findByRole("dialog", { name: title });
  return calls;
}

describe("Wardrobe", () => {
  beforeEach(() => resetWardrobeState());
  afterEach(() => {
    closeWardrobe();
    resetWardrobeState();
  });

  it("has Outfit, Hair and Colour tabs, Custom Cast style, with arrow keys", async () => {
    await show();
    const tabs = within(screen.getByRole("tablist", { name: "Wardrobe" })).getAllByRole("tab");
    expect(tabs.map((t) => t.textContent)).toEqual(["Outfit", "Hair", "Colour"]);
    expect(tabs[0]).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(tabs[0]!, { key: "ArrowRight" });
    expect(tab("Hair")).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(tab("Hair"), { key: "End" });
    expect(tab("Colour")).toHaveAttribute("aria-selected", "true");
  });

  it("shows the outfits with the one she wears framed, and puts one on with a tap", async () => {
    const calls = await show();
    const tiles = within(grid()).getAllByRole("button", { name: /^Put on/ });
    expect(tiles.map((b) => b.getAttribute("aria-label"))).toEqual(["Put on Uniform", "Put on Gym", "Put on Chef", "Put on Maid"]);
    expect(tiles[0]).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByText("Wearing")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Put on Maid" }));
    await waitFor(() => expect(puts(calls)).toEqual([{ outfit: "maid" }]));
    expect(screen.getByRole("button", { name: "Put on Maid" })).toHaveAttribute("aria-pressed", "true");
    // Pinned: the chip unpins outfit and hair.
    const pin = screen.getByRole("button", { name: /Pinned/ });
    await waitFor(() => expect(pin).not.toBeDisabled());
    fireEvent.click(pin);
    await waitFor(() => expect(puts(calls)).toEqual([{ outfit: "maid" }, { outfit: null, hair: null }]));
    expect(avatarSettings().outfit).toBeNull();
  });

  it("filters outfits by category", async () => {
    await show();
    const chips = screen.getByRole("radiogroup", { name: "Categories" });
    expect(within(chips).getAllByRole("radio").map((t) => t.textContent)).toEqual(["All", "Uniform", "Sport", "Work"]);
    fireEvent.click(within(chips).getByRole("radio", { name: "Work" }));
    expect(within(grid()).getAllByRole("button").map((b) => b.getAttribute("aria-label"))).toEqual(["Put on Chef", "Put on Maid"]);
  });

  it("scrolls only the body under a fixed header, with full names on the tiles", async () => {
    await show();
    const dialog = screen.getByRole("dialog", { name: "Wardrobe" });
    const body = dialog.querySelector(".wardrobe-body")!;
    expect(body).toContainElement(grid());
    expect(body).not.toContainElement(screen.getByRole("heading", { name: "Wardrobe" }));
    expect(screen.getByRole("button", { name: "Put on Maid" })).toHaveAttribute("title", "Maid");
  });

  it("fades the edge of the chip row that has more chips behind it", async () => {
    await show();
    const chips = screen.getByRole("radiogroup", { name: "Categories" });
    const size = (scrollLeft: number) => {
      Object.defineProperty(chips, "scrollWidth", { configurable: true, value: 500 });
      Object.defineProperty(chips, "clientWidth", { configurable: true, value: 300 });
      chips.scrollLeft = scrollLeft;
      fireEvent.scroll(chips);
    };
    size(0);
    expect(chips).toHaveClass("wchips--more-end");
    expect(chips).not.toHaveClass("wchips--more-start");
    size(100);
    expect(chips).toHaveClass("wchips--more-start", "wchips--more-end");
    size(200);
    expect(chips).not.toHaveClass("wchips--more-end");
  });

  it("styles her hair from the Hair tab, her own hair first", async () => {
    const calls = await show();
    fireEvent.click(tab("Hair"));
    const tiles = within(grid()).getAllByRole("button");
    expect(tiles.map((b) => b.getAttribute("aria-label"))).toEqual([
      "Style her hair: Natural",
      "Style her hair: Ponytail",
      "Style her hair: Bun",
    ]);
    expect(tiles[0]).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(tiles[2]!);
    await waitFor(() => expect(puts(calls)).toEqual([{ hair: "bun" }]));
    expect(screen.getByRole("button", { name: "Style her hair: Bun" })).toHaveAttribute("aria-pressed", "true");
    setAvatarPath("/gym");
    expect(screen.getByRole("button", { name: "Style her hair: Bun" }), "a pin wins over the gym ponytail").toHaveAttribute("aria-pressed", "true");
  });

  it("tints her hair from the Colour tab", async () => {
    const calls = await show();
    fireEvent.click(tab("Colour"));
    const swatches = within(screen.getByRole("radiogroup", { name: "Colour" })).getAllByRole("radio");
    expect(swatches.map((s) => s.textContent)).toEqual(["Natural", "Pink", "Silver"]);
    expect(swatches[0]).toHaveAttribute("aria-checked", "true");
    fireEvent.click(swatches[1]!);
    await waitFor(() => expect(puts(calls)).toEqual([{ hairColor: "pink" }]));
    expect(swatches[1]).toHaveAttribute("aria-checked", "true");
    fireEvent.click(swatches[0]!);
    await waitFor(() => expect(puts(calls)).toEqual([{ hairColor: "pink" }, { hairColor: null }]));
  });

  it("switches auto off and picks per-section outfits and hair", async () => {
    const calls = await show();
    const sw = screen.getByRole("switch", { name: "Change by section" });
    expect(sw).toHaveAttribute("aria-checked", "true");
    fireEvent.click(sw);
    await waitFor(() => expect(puts(calls)).toEqual([{ auto: false }]));
    expect(sw).toHaveAttribute("aria-checked", "false");

    const shopping = screen.getByRole("radiogroup", { name: "Shopping" });
    expect(within(shopping).getByRole("radio", { name: "Shopping: Chef" })).toHaveAttribute("aria-checked", "true");
    fireEvent.click(within(shopping).getByRole("radio", { name: "Shopping: Maid" }));
    await waitFor(() => expect(puts(calls)).toContainEqual({ rules: { shopping: { outfit: "maid" } } }));

    const gymHair = screen.getByRole("radiogroup", { name: "Gym: hair" });
    expect(within(gymHair).getByRole("radio", { name: "Gym hair: Ponytail" })).toHaveAttribute("aria-checked", "true");
    fireEvent.click(within(gymHair).getByRole("radio", { name: "Gym hair: Bun" }));
    await waitFor(() => expect(puts(calls)).toContainEqual({ rules: { gym: { hair: "bun" } } }));
    expect(within(gymHair).getByRole("radio", { name: "Gym hair: Bun" })).toHaveAttribute("aria-checked", "true");
  });

  it("follows the section on screen", async () => {
    await show();
    setAvatarPath("/gym");
    expect(screen.getByRole("button", { name: "Put on Gym" })).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(tab("Hair"));
    expect(screen.getByRole("button", { name: "Style her hair: Ponytail" })).toHaveAttribute("aria-pressed", "true");
  });

  it("opens the COM3D2 fitting room behind an off-by-default switch, remembered on this device", async () => {
    localStorage.removeItem(COM3D2_KEY);
    await show();
    const sw = screen.getByRole("switch", { name: "COM3D2 body (beta)" });
    expect(sw).toHaveAttribute("aria-checked", "false");
    expect(screen.queryByTitle("COM3D2 fitting room")).toBeNull();

    fireEvent.click(sw);
    expect(sw).toHaveAttribute("aria-checked", "true");
    const frame = screen.getByTitle("COM3D2 fitting room");
    expect(frame.tagName).toBe("IFRAME");
    expect(frame).toHaveAttribute("src", COM3D2_URL);
    expect(screen.queryByRole("tablist", { name: "Wardrobe" })).toBeNull();
    expect(screen.getByRole("dialog")).toHaveClass("wardrobe-panel--wide");
    expect(localStorage.getItem(COM3D2_KEY)).toBe("1");

    fireEvent.click(sw);
    expect(screen.queryByTitle("COM3D2 fitting room")).toBeNull();
    expect(tab("Outfit")).toBeInTheDocument();
    expect(localStorage.getItem(COM3D2_KEY)).toBeNull();
  });

  it("closes with Escape and the close button", async () => {
    await show();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(wardrobeOpen()).toBe(false);
    openWardrobe();
    fireEvent.click(await screen.findByRole("button", { name: "Close the wardrobe" }));
    expect(wardrobeOpen()).toBe(false);
  });

  it("says so when nothing is installed, and reads the old bare array", async () => {
    await show({ outfits: [], hairstyles: [], colors: [] });
    expect(screen.getByText("No outfits yet")).toBeInTheDocument();
    fireEvent.click(tab("Hair"));
    expect(screen.getByText(/Hairstyles appear here/)).toBeInTheDocument();
    closeWardrobe();
    resetWardrobeState();
  });

  it("speaks Italian and Danish", async () => {
    setLocale("it");
    await show(undefined, "Guardaroba");
    expect(screen.getByRole("button", { name: "Indossa Divisa" })).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "Cambia in base alla sezione" })).toBeInTheDocument();
    expect(within(screen.getByRole("tablist", { name: "Guardaroba" })).getAllByRole("tab").map((t) => t.textContent)).toEqual([
      "Vestito",
      "Capelli",
      "Colore",
    ]);
    setLocale("da");
    expect(screen.getByRole("dialog", { name: "Garderobe" })).toBeInTheDocument();
    setLocale(null);
  });
});
