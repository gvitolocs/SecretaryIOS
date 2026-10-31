import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { Route } from "@solidjs/router";
import { beforeEach, describe, expect, it } from "vitest";
import { setLocale } from "../i18n";
import { resetShopping, shop } from "../state/shopping";
import { renderAt } from "../test/router";
import { stubFetch } from "../test/fetch";
import { toggleChip } from "./shopping/DetailsSheet";
import { fakeShoppingServer, testItem, type FakeServer } from "./shopping/fakeServer";
import ShoppingView from "./Shopping";

let srv: FakeServer;

function tileState(name: RegExp): string | null {
  const el = screen.getByRole("button", { name }).closest(".tile");
  if (!el) return null;
  return el.classList.contains("tile--need") ? "need" : el.classList.contains("tile--recent") ? "recent" : "other";
}

const lists = () => screen.queryAllByRole("list").map((l) => l.getAttribute("aria-label"));
const tileNames = (list: string) =>
  Array.from(screen.getByRole("list", { name: list }).querySelectorAll("[data-tile]")).map((b) => b.getAttribute("aria-label") ?? "");
const opTypes = () => srv.opsPosts().flatMap((p) => p.ops.map((o) => o.type));

async function renderView(firstTile = /^Latte, da comprare/) {
  renderAt("/shopping", () => <Route path="/shopping" component={ShoppingView} />);
  await screen.findByRole("button", { name: firstTile });
  await waitFor(() => expect(shop.pending).toBe(0));
  await waitFor(() => expect(shop.lists.length).toBe(2));
  await waitFor(() => expect(shop.catalog.items.length).toBeGreaterThan(0));
}

describe("Shopping view (Bring layout)", () => {
  beforeEach(() => {
    setLocale("it");
    localStorage.clear();
    resetShopping();
    srv = fakeShoppingServer({
      default: {
        name: "Casa",
        theme: "home",
        items: [
          testItem({ id: "milk-row", name: "Latte", catalogId: "milk", section: "dairy", updatedBy: "telegram" }),
          testItem({ id: "banana-row", name: "Banane", catalogId: "banana", section: "produce", spec: "mature" }),
          testItem({ id: "bread-row", name: "Pane", status: "recent" }),
        ],
      },
      hardware: { name: "Ferramenta", theme: "hardware", items: [testItem({ id: "screw-row", name: "Viti", spec: "4x30" })] },
    });
    stubFetch(srv.handler);
  });

  it("lays out the home screen: list switcher, one to-buy grid in store order, Recently, then collapsed catalogue sections", async () => {
    await renderView();
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Casa");
    expect(screen.getByRole("button", { name: "Liste, lista attuale Casa" })).toBeInTheDocument();
    expect(screen.getByRole("status", { name: "2 da comprare" })).toHaveTextContent("2");
    expect(lists()).toEqual(["Da comprare", "Usati di recente"]);
    // To buy is one grid, sorted by the store walk (produce before dairy), whatever the row order.
    expect(tileNames("Da comprare")).toEqual([expect.stringMatching(/^Banane/), expect.stringMatching(/^Latte/)]);
    expect(screen.getByText("mature")).toBeInTheDocument();
    // Catalogue sections start collapsed and show how many of their items are to buy.
    const dairy = screen.getByRole("button", { name: "Latticini, 1 in lista", expanded: false });
    fireEvent.click(dairy);
    expect(dairy).toHaveAttribute("aria-expanded", "true");
    const section = screen.getByRole("list", { name: "Latticini" });
    expect(within(section).getByRole("button", { name: /^Latte, da comprare/ }).closest(".tile")).toHaveClass("tile--need");
    expect(within(section).getByRole("button", { name: "Latte d'avena. Tocca per aggiungerlo." }).closest(".tile")).toHaveClass("tile--catalog");

    // Section headers inside "To buy" are a list setting.
    fireEvent.click(screen.getByRole("button", { name: "Impostazioni lista" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Titoli dei reparti" }));
    expect(lists().slice(0, 3)).toEqual(["Frutta e verdura", "Latticini", "Usati di recente"]);
  });

  it("tap moves a tile to Recently and back, with undo", async () => {
    await renderView();
    fireEvent.click(screen.getByRole("button", { name: /^Latte, da comprare/ }));
    expect(tileState(/^Latte\. Tocca per rimetterlo/)).toBe("recent");
    expect(screen.getByText("Latte comprato")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Annulla" }));
    expect(tileState(/^Latte, da comprare/)).toBe("need");
    expect(screen.queryByText("Latte comprato")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /^Pane\. Tocca per rimetterlo/ }));
    expect(tileState(/^Pane, da comprare/)).toBe("need");
    await waitFor(() => expect(shop.pending).toBe(0));
    expect(opTypes()).toEqual(["purchase", "need", "need"]);
  });

  it("shows “Nothing to buy!” and edits Recently (select + remove, clear all)", async () => {
    await renderView();
    fireEvent.click(screen.getByRole("button", { name: /^Latte, da comprare/ }));
    fireEvent.click(screen.getByRole("button", { name: /^Banane \(mature\), da comprare/ }));
    expect(screen.getByText("Niente da comprare!")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Altre azioni per Usati di recente" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Modifica" }));
    const remove = screen.getByRole("button", { name: "Rimuovi" });
    expect(remove).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: /^Pane, tocca per selezionare/ }));
    expect(screen.getByRole("button", { name: /^Pane, selezionato/ })).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(screen.getByRole("button", { name: "Rimuovi (1)" }));
    expect(screen.queryByRole("button", { name: /^Pane/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Rimuovi/ })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Altre azioni per Usati di recente" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Svuota tutto" }));
    expect(screen.queryByRole("list", { name: "Usati di recente" })).toBeNull();
    await waitFor(() => expect(shop.pending).toBe(0));
    expect(opTypes()).toEqual(["purchase", "purchase", "remove", "clear_recent"]);
    expect(srv.lists.get("default")!.items.filter((i) => i.status !== "removed")).toHaveLength(0);
  });

  it("searches from the top: suggestions as tiles, quick specs, the list's own items, custom text", async () => {
    await renderView();
    const input = screen.getByRole("searchbox", { name: "Cerca o aggiungi un articolo" });

    // Typing swaps the home grids for suggestions; tapping one adds it and brings the list back.
    fireEvent.input(input, { target: { value: "caffe" } });
    expect(screen.queryByRole("list", { name: "Da comprare" })).toBeNull();
    let results = screen.getByRole("list", { name: "Suggerimenti" });
    fireEvent.click(within(results).getByRole("button", { name: "Caffè. Tocca per aggiungerlo." }));
    expect(input).toHaveValue("");
    expect(within(screen.getByRole("list", { name: "Da comprare" })).getByRole("button", { name: /^Caffè, da comprare/ })).toBeInTheDocument();

    // A spec typed after the name goes on the row.
    fireEvent.input(input, { target: { value: "pasta x2" } });
    results = screen.getByRole("list", { name: "Suggerimenti" });
    expect(within(results).getByText("x2")).toBeInTheDocument();
    fireEvent.click(within(results).getByRole("button", { name: "Pasta. Tocca per aggiungerlo." }));
    expect(screen.getByRole("button", { name: /^Pasta \(x2\), da comprare/ })).toBeInTheDocument();

    // Free text with a quick-spec chip, added with Enter.
    fireEvent.input(input, { target: { value: "Candele" } });
    fireEvent.click(screen.getByRole("button", { name: "bio" }));
    expect(screen.getByRole("button", { name: "Aggiungi «Candele (bio)»" }).closest(".tile")).toHaveClass("tile--custom");
    fireEvent.submit(input.closest("form")!);
    expect(input).toHaveValue("");
    expect(screen.getByRole("button", { name: /^Candele \(bio\), da comprare/ })).toBeInTheDocument();

    // The owner's own items come back as suggestions; Escape clears the search.
    fireEvent.input(input, { target: { value: "cand" } });
    results = screen.getByRole("list", { name: "Suggerimenti" });
    expect(within(results).getByRole("button", { name: /^Candele, da comprare/ })).toBeInTheDocument();
    expect(within(results).queryByRole("button", { name: /^Aggiungi/ })).toBeInTheDocument();
    fireEvent.keyDown(input, { key: "Escape" });
    expect(input).toHaveValue("");
    expect(screen.getByRole("list", { name: "Da comprare" })).toBeInTheDocument();

    await waitFor(() => expect(shop.pending).toBe(0));
    expect(srv.opsPosts().flatMap((p) => p.ops)).toEqual([
      expect.objectContaining({ type: "add", catalogId: "coffee", name: "Caffè" }),
      expect.objectContaining({ type: "add", catalogId: "pasta", name: "Pasta", spec: "x2" }),
      expect.objectContaining({ type: "add", name: "Candele", spec: "bio" }),
    ]);
  });

  it("orders sections by the store walk: the to-buy grid and the catalogue follow it", async () => {
    await renderView();
    fireEvent.click(screen.getByRole("button", { name: "Ordine dei reparti" }));
    const dialog = screen.getByRole("dialog", { name: "Ordine dei reparti" });
    expect(within(dialog).getByRole("button", { name: "Sposta Frutta e verdura su" })).toBeDisabled();
    fireEvent.click(within(dialog).getByRole("button", { name: "Sposta Latticini su" }));
    expect(within(dialog).getAllByRole("listitem").map((li) => li.textContent)).toEqual([
      expect.stringContaining("Latticini"),
      expect.stringContaining("Frutta e verdura"),
      expect.stringContaining("Dispensa"),
    ]);
    fireEvent.click(within(dialog).getByRole("button", { name: "Fatto" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(tileNames("Da comprare")).toEqual([expect.stringMatching(/^Latte/), expect.stringMatching(/^Banane/)]);
    const headers = screen.getAllByRole("button", { expanded: false }).map((b) => b.textContent);
    expect(headers.indexOf("Latticini1")).toBeLessThan(headers.indexOf("Frutta e verdura1"));
    expect(JSON.parse(localStorage.getItem("secretary.shopping.sectionOrder") ?? "{}")).toEqual({
      default: ["dairy", "produce", "pantry"],
    });
  });

  it("names catalogue items in the list's language, also under another UI language", async () => {
    setLocale("en");
    await renderView(/^Latte, to buy/);
    const input = screen.getByRole("searchbox", { name: "Search or add an item" });
    fireEvent.input(input, { target: { value: "oat" } });
    // The list's rows are Italian, so the suggestion is too, and "oat" still finds it.
    expect(screen.getByRole("button", { name: "Latte d'avena. Tap to add." })).toBeInTheDocument();
    fireEvent.keyDown(input, { key: "Escape" });
    fireEvent.click(screen.getByRole("button", { name: "List settings" }));
    fireEvent.change(screen.getByLabelText("Item names"), { target: { value: "en" } });
    fireEvent.input(input, { target: { value: "oat" } });
    expect(screen.getByRole("button", { name: "Oat milk. Tap to add." })).toBeInTheDocument();
  });

  it("edits an item in the details sheet and saves on Done", async () => {
    await renderView();
    fireEvent.click(screen.getByRole("button", { name: "Dettagli di Latte" }));
    const dialog = screen.getByRole("dialog", { name: "Latte" });
    expect(within(dialog).getByText("via Telegram")).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "x6" }));
    fireEvent.click(within(dialog).getByRole("button", { name: "senza lattosio" }));
    expect(within(dialog).getByPlaceholderText("Aggiungi una specifica")).toHaveValue("x6, senza lattosio");
    fireEvent.input(within(dialog).getByLabelText("Quantità"), { target: { value: "2" } });
    fireEvent.input(within(dialog).getByLabelText("Unità"), { target: { value: "l" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Cambia reparto" }));
    fireEvent.click(within(dialog).getByRole("radio", { name: /Dispensa/ }));
    fireEvent.click(within(dialog).getByRole("button", { name: "Fatto" }));

    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.getByText("2 l · x6, senza lattosio")).toBeInTheDocument();
    expect(shop.items.find((i) => i.id === "milk-row")?.section).toBe("pantry");
    await waitFor(() => expect(shop.pending).toBe(0));
    expect(srv.opsPosts().at(-1)!.ops).toEqual([
      expect.objectContaining({ type: "set_spec", itemId: "milk-row", spec: "x6, senza lattosio" }),
      expect.objectContaining({ type: "set_quantity", itemId: "milk-row", quantity: 2, unit: "l" }),
      expect.objectContaining({ type: "set_section", itemId: "milk-row", section: "pantry" }),
    ]);
  });

  it("opens details with a long-press, and deletes and moves items", async () => {
    await renderView();
    const banana = screen.getByRole("button", { name: /^Banane \(mature\)/ });
    fireEvent.contextMenu(banana);
    fireEvent.click(within(screen.getByRole("dialog", { name: "Banane" })).getByRole("button", { name: "Elimina articolo" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByRole("button", { name: /^Banane/ })).toBeNull();
    // Undo re-adds it as a new row.
    fireEvent.click(screen.getByRole("button", { name: "Annulla" }));
    expect(screen.getByRole("button", { name: /^Banane \(mature\), da comprare/ })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Dettagli di Latte" }));
    const dialog = screen.getByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Sposta articolo" }));
    fireEvent.click(within(dialog).getByRole("button", { name: /Ferramenta/ }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByRole("button", { name: /^Latte/ })).toBeNull();
    await waitFor(() => expect(shop.pending).toBe(0));
    expect(opTypes()).toEqual(["remove", "add", "move"]);
  });

  it("switches, renames, deletes and creates lists in the list chooser", async () => {
    await renderView();
    fireEvent.click(screen.getByRole("button", { name: "Liste, lista attuale Casa" }));
    let chooser = screen.getByRole("dialog", { name: "Liste" });
    expect(within(chooser).getByRole("button", { name: "Apri Casa, 2 da comprare" })).toHaveAttribute("aria-current", "true");
    fireEvent.click(within(chooser).getByRole("button", { name: "Apri Ferramenta, 1 da comprare" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(await screen.findByRole("button", { name: /^Viti \(4x30\), da comprare/ })).toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Ferramenta");

    // Rename and re-theme via the gear.
    fireEvent.click(screen.getByRole("button", { name: "Liste, lista attuale Ferramenta" }));
    chooser = screen.getByRole("dialog", { name: "Liste" });
    fireEvent.click(within(chooser).getByRole("button", { name: "Impostazioni di Ferramenta" }));
    const form = within(chooser).getByRole("form", { name: "Impostazioni di Ferramenta" });
    fireEvent.input(within(form).getByLabelText("Nome della lista"), { target: { value: "Brico" } });
    fireEvent.change(within(form).getByLabelText("Tema"), { target: { value: "garden" } });
    fireEvent.click(within(form).getByRole("button", { name: "Salva" }));
    await waitFor(() => expect(within(chooser).getByRole("button", { name: "Apri Brico, 1 da comprare" })).toBeInTheDocument());
    expect(srv.calls.find((c) => c.method === "PATCH")?.body).toEqual({ name: "Brico", theme: "garden" });

    // The default list can't be deleted; another one can, in two steps.
    fireEvent.click(within(chooser).getByRole("button", { name: "Impostazioni di Casa" }));
    expect(within(chooser).getByText("La lista principale non si può eliminare.")).toBeInTheDocument();
    fireEvent.click(within(chooser).getByRole("button", { name: "Impostazioni di Brico" }));
    fireEvent.click(within(chooser).getByRole("button", { name: "Elimina lista" }));
    fireEvent.click(within(chooser).getByRole("button", { name: "Elimina lista" }));
    await waitFor(() => expect(within(chooser).queryByRole("button", { name: /Apri Brico/ })).toBeNull());
    expect(srv.lists.has("hardware")).toBe(false);
    expect(within(chooser).getByRole("button", { name: /Apri Casa/ })).toHaveAttribute("aria-current", "true");

    // Create a list: it becomes current and starts empty.
    fireEvent.click(within(chooser).getByRole("button", { name: "Crea lista" }));
    fireEvent.input(within(chooser).getByLabelText("Nome della lista"), { target: { value: "Vacanze" } });
    fireEvent.click(within(chooser).getByRole("button", { name: "Crea" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Vacanze");
    expect(await screen.findByText("Niente da comprare!")).toBeInTheDocument();
    expect(srv.calls.find((c) => c.method === "POST" && c.url === "/v1/shopping/lists")?.body).toEqual({ name: "Vacanze", theme: "grocery" });
  });

  it("switches the UI language from the list settings", async () => {
    await renderView();
    fireEvent.click(screen.getByRole("button", { name: "Impostazioni lista" }));
    fireEvent.change(screen.getByLabelText("Lingua"), { target: { value: "en" } });
    expect(await screen.findByRole("button", { name: /^Latte, to buy/ })).toBeInTheDocument();
    expect(screen.getByText("Recently used")).toBeInTheDocument();
    await waitFor(() => expect(srv.calls.some((c) => c.url === "/v1/shopping/catalog?lang=en")).toBe(true));
  });

  it("moves focus between tiles with the arrow keys", async () => {
    await renderView();
    const tiles = Array.from(document.querySelectorAll<HTMLElement>(".bring-board [data-tile]"));
    tiles[0]!.focus();
    fireEvent.keyDown(tiles[0]!, { key: "ArrowRight" });
    expect(document.activeElement).toBe(tiles[1]);
    fireEvent.keyDown(tiles[1]!, { key: "End" });
    expect(document.activeElement).toBe(tiles.at(-1));
  });
});

describe("helpers", () => {
  it("toggleChip adds and removes chips; quantity chips replace each other", () => {
    expect(toggleChip("", "bio")).toBe("bio");
    expect(toggleChip("bio", "x2")).toBe("bio, x2");
    expect(toggleChip("bio, x2", "x6")).toBe("bio, x6");
    expect(toggleChip("bio, x6", "bio")).toBe("x6");
  });
});
