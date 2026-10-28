import { Route, Router } from "@solidjs/router";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { setShowAdult } from "../lib/adult";
import PluginsView, { PLUGINS, visiblePlugins } from "./Plugins";

function renderPlugins() {
  render(() => (
    <Router>
      <Route path="*" component={PluginsView} />
    </Router>
  ));
}

describe("Plugins: NClient card", () => {
  afterEach(() => { vi.restoreAllMocks(); setShowAdult(true); });

  it("is an adult plugin that opens /nclient", () => {
    const entry = PLUGINS.find((p) => p.id === "nclient");
    expect(entry).toMatchObject({ href: "/nclient", adult: true });
  });

  it("shows the card while adult plugins are on, and opens NClient from it", async () => {
    setShowAdult(true);
    renderPlugins();
    const card = screen.getByRole("button", { name: /NClient/ });
    expect(card).toHaveAttribute("data-plugin", "nclient");
    fireEvent.click(card);
    await waitFor(() => expect(window.location.pathname).toBe("/nclient"));
  });

  it("hides the card when the adult switch is off, and brings it back when it is on again", () => {
    setShowAdult(false);
    expect(visiblePlugins().some((p) => p.id === "nclient")).toBe(false);
    renderPlugins();
    expect(screen.queryByRole("button", { name: /NClient/ })).toBeNull();
    // The other cards stay.
    expect(screen.getByRole("button", { name: /Films/ })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("checkbox", { name: /Show adult plugins/ }));
    expect(screen.getByRole("button", { name: /NClient/ })).toBeInTheDocument();
  });
});
