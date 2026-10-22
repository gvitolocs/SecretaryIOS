import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { setLocale } from "../i18n";
import { stubFetch, type Call } from "../test/fetch";
import { OUTFITS, settings } from "../test/wardrobe";
import { avatarSettings, closeWardrobe, loadAvatar, openWardrobe, resetWardrobeState, setAvatarPath, wardrobeOpen } from "../state/wardrobe";
import Wardrobe from "./Wardrobe";

function serve(list: unknown[] = OUTFITS): Call[] {
  let current = settings();
  return stubFetch((c) => {
    if (c.url === "/v1/avatar/wardrobe") return { body: list };
    if (c.url === "/v1/avatar" && c.method === "GET") return { body: current };
    if (c.url === "/v1/avatar" && c.method === "PUT") {
      const p = c.body as Record<string, unknown>;
      current = { ...current, ...p, rules: { ...current.rules, ...((p.rules as object) ?? {}) } } as typeof current;
      return { body: current };
    }
    return { status: 404 };
  });
}

const puts = (calls: Call[]) => calls.filter((c) => c.method === "PUT").map((c) => c.body);

async function show(list?: unknown[], title = "Wardrobe") {
  const calls = serve(list);
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

  it("shows the outfits with the one she wears framed, and puts one on with a tap", async () => {
    const calls = await show();
    const grid = screen.getByRole("tabpanel", { name: "Outfits" });
    const tiles = within(grid).getAllByRole("button");
    expect(tiles.map((b) => b.getAttribute("aria-label"))).toEqual(["Put on Uniform", "Put on Gym", "Put on Chef", "Put on Maid"]);
    expect(tiles[0]).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByText("Wearing")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Put on Maid" }));
    await waitFor(() => expect(puts(calls)).toEqual([{ outfit: "maid" }]));
    expect(screen.getByRole("button", { name: "Put on Maid" })).toHaveAttribute("aria-pressed", "true");
    // Pinned: the chip unpins.
    const pin = screen.getByRole("button", { name: /Pinned/ });
    await waitFor(() => expect(pin).not.toBeDisabled());
    fireEvent.click(pin);
    await waitFor(() => expect(puts(calls)).toEqual([{ outfit: "maid" }, { outfit: null }]));
    expect(avatarSettings().outfit).toBeNull();
  });

  it("filters by category tabs, with arrow keys", async () => {
    await show();
    const tabs = screen.getByRole("tablist", { name: "Categories" });
    expect(within(tabs).getAllByRole("tab").map((t) => t.textContent)).toEqual(["All", "Uniform", "Sport", "Work"]);
    fireEvent.click(within(tabs).getByRole("tab", { name: "Work" }));
    const grid = screen.getByRole("tabpanel");
    expect(within(grid).getAllByRole("button").map((b) => b.getAttribute("aria-label"))).toEqual(["Put on Chef", "Put on Maid"]);
    fireEvent.keyDown(within(tabs).getByRole("tab", { name: "Work" }), { key: "ArrowRight" });
    expect(within(tabs).getByRole("tab", { name: "All" })).toHaveAttribute("aria-selected", "true");
  });

  it("switches auto off and picks per-section outfits", async () => {
    const calls = await show();
    const sw = screen.getByRole("switch", { name: "Change by section" });
    expect(sw).toHaveAttribute("aria-checked", "true");
    fireEvent.click(sw);
    await waitFor(() => expect(puts(calls)).toEqual([{ auto: false }]));
    expect(sw).toHaveAttribute("aria-checked", "false");

    const shopping = screen.getByRole("radiogroup", { name: "Shopping" });
    expect(within(shopping).getByRole("radio", { name: "Shopping: Chef" })).toHaveAttribute("aria-checked", "true");
    fireEvent.click(within(shopping).getByRole("radio", { name: "Shopping: Maid" }));
    await waitFor(() => expect(puts(calls)).toContainEqual({ rules: { shopping: "maid" } }));
    expect(within(shopping).getByRole("radio", { name: "Shopping: Maid" })).toHaveAttribute("aria-checked", "true");
  });

  it("follows the section on screen", async () => {
    await show();
    setAvatarPath("/gym");
    expect(screen.getByRole("button", { name: "Put on Gym" })).toHaveAttribute("aria-pressed", "true");
  });

  it("closes with Escape and the close button", async () => {
    await show();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(wardrobeOpen()).toBe(false);
    openWardrobe();
    fireEvent.click(await screen.findByRole("button", { name: "Close the wardrobe" }));
    expect(wardrobeOpen()).toBe(false);
  });

  it("says so when no wardrobe is installed", async () => {
    await show([]);
    expect(screen.getByText("No outfits yet")).toBeInTheDocument();
    expect(screen.queryByRole("tablist")).toBeNull();
  });

  it("speaks Italian and Danish", async () => {
    setLocale("it");
    await show(undefined, "Guardaroba");
    expect(screen.getByRole("dialog", { name: "Guardaroba" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Indossa Divisa" })).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "Cambia in base alla sezione" })).toBeInTheDocument();
    setLocale("da");
    expect(screen.getByRole("dialog", { name: "Garderobe" })).toBeInTheDocument();
    setLocale(null);
  });
});
