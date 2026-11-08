import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { Route } from "@solidjs/router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { resetShopping, shop } from "../state/shopping";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import { fakeShoppingServer, testItem, type FakeServer } from "./shopping/fakeServer";
import ShoppingView from "./Shopping";

let srv: FakeServer;

function serve(casa = [testItem({ id: "milk-row", name: "Latte", catalogId: "milk", section: "dairy" }), testItem({ id: "bread-row", name: "Pane", status: "recent" })]) {
  srv = fakeShoppingServer({
    default: { name: "Casa", theme: "home", items: casa },
    hardware: { name: "Ferramenta", theme: "hardware", items: [] },
  });
  stubFetch(srv.handler);
}

async function renderView() {
  renderAt("/shopping", () => <Route path="/shopping" component={ShoppingView} />);
  await screen.findByRole("heading", { level: 1, name: "Spesa" });
  await waitFor(() => expect(shop.loaded).toBe(true));
  await waitFor(() => expect(shop.catalog.items.length).toBeGreaterThan(0));
  await waitFor(() => expect(shop.lists.length).toBe(2));
}

describe("Shopping desktop layout", () => {
  beforeEach(() => {
    setLocale("it");
    localStorage.clear();
    resetShopping();
    stubWide();
  });
  afterEach(() => vi.unstubAllGlobals());

  it("uses the page header, a main column of cards and a rail instead of the phone toolbar", async () => {
    serve();
    await renderView();
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Spesa");
    expect(document.querySelector(".pg-context")).toHaveTextContent("Casa · 1 da comprare");
    // The phone bar, list banner and red add button are gone; there is no accent button at all.
    expect(document.querySelector(".shop-head")).toBeNull();
    expect(document.querySelector(".shop-list-btn")).toBeNull();
    expect(document.querySelector(".shop-search-add")).toBeNull();
    expect(document.querySelectorAll(".pg-cta").length).toBeLessThanOrEqual(1);
    // Main column: search, then the to-buy card and the catalogue card.
    expect(screen.getByRole("searchbox", { name: "Cerca o aggiungi un articolo" })).toHaveClass("pg-input");
    const toBuy = screen.getByRole("region", { name: "Da comprare" });
    expect(within(toBuy).getByRole("button", { name: /^Latte, da comprare/ })).toBeInTheDocument();
    const catalog = screen.getByRole("region", { name: "Catalogo" });
    expect(within(catalog).getByRole("button", { name: "Ordine dei reparti" })).toBeInTheDocument();
    // Rail: recently used and the offers link.
    const rail = screen.getByRole("complementary", { name: "Usati di recente" });
    expect(within(rail).getByRole("button", { name: /^Pane\. Tocca per rimetterlo/ })).toBeInTheDocument();
    expect(within(rail).getByRole("link", { name: "Mappa delle offerte" })).toHaveAttribute("href", "/shopping/map");
  });

  it("keeps the interactions: tap to buy and back, list switcher in a dialog, details dialog with a two-step delete", async () => {
    serve();
    await renderView();
    fireEvent.click(screen.getByRole("button", { name: /^Latte, da comprare/ }));
    expect(screen.getByRole("button", { name: /^Latte\. Tocca per rimetterlo/ })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /^Latte\. Tocca per rimetterlo/ }));
    expect(screen.getByRole("button", { name: /^Latte, da comprare/ })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Liste, lista attuale Casa" }));
    const lists = screen.getByRole("dialog", { name: "Liste" });
    fireEvent.click(within(lists).getByRole("button", { name: "Apri Ferramenta, 0 da comprare" }));
    await waitFor(() => expect(document.querySelector(".pg-context")).toHaveTextContent("Ferramenta"));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("opens item details in a dialog and asks twice before deleting", async () => {
    serve();
    await renderView();
    fireEvent.click(screen.getByRole("button", { name: "Dettagli di Latte" }));
    const dialog = screen.getByRole("dialog", { name: "Latte" });
    expect(within(dialog).getAllByRole("button", { name: "Fatto" })).toHaveLength(1);
    fireEvent.click(within(dialog).getByRole("button", { name: "Elimina articolo" }));
    // Armed: a danger button and Cancel; nothing is deleted yet.
    expect(srv.opsPosts().flatMap((p) => p.ops)).toHaveLength(0);
    fireEvent.click(within(dialog).getByRole("button", { name: "Annulla" }));
    expect(within(dialog).getByRole("button", { name: "Elimina articolo" })).not.toHaveClass("pg-btn--danger");
    fireEvent.click(within(dialog).getByRole("button", { name: "Elimina articolo" }));
    fireEvent.click(within(dialog).getByRole("group", { name: "Elimina articolo" }).querySelector(".pg-btn--danger")!);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(screen.queryByRole("button", { name: /^Latte, da comprare/ })).toBeNull();
  });

  it("shows a friendly empty state when nothing is to buy", async () => {
    serve([]);
    await renderView();
    expect(screen.getByText("Niente da comprare!")).toBeInTheDocument();
    expect(document.querySelector(".pg-empty")).not.toBeNull();
    expect(document.querySelector(".bring-empty")).toBeNull();
  });

  it("keeps the header and shows an error card with Try again when the list cannot load", async () => {
    stubFetch(() => ({ status: 500, body: { error: { code: "boom", message: "no" } } }));
    renderAt("/shopping", () => <Route path="/shopping" component={ShoppingView} />);
    expect(await screen.findByRole("heading", { level: 1, name: "Spesa" })).toBeInTheDocument();
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveClass("pg-error");
    expect(within(alert).getByRole("button", { name: "Riprova" })).toBeInTheDocument();
  });
});
