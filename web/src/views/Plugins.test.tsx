import { Route, Router } from "@solidjs/router";
import { render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import PluginsView from "./Plugins";

function renderPlugins() {
  render(() => (
    <Router>
      <Route path="*" component={PluginsView} />
    </Router>
  ));
}

describe("Plugins", () => {
  afterEach(() => vi.restoreAllMocks());

  it("lists the plugin cards", () => {
    renderPlugins();
    for (const name of ["Voice memos", "Notes", "Tasks", "Connections", "Pokémon", "Wardrobe"]) {
      expect(screen.getByRole("button", { name: new RegExp(name) })).toBeInTheDocument();
    }
  });

  it("shows the Phone card only inside the Android app", () => {
    renderPlugins();
    expect(screen.queryByRole("button", { name: /Phone/ })).toBeNull();
  });

  it("shows the Phone card when the user agent is the Android app's", () => {
    vi.spyOn(navigator, "userAgent", "get").mockReturnValue("Mozilla/5.0 (Linux; Android 15) SecretaryAndroid/0.2.0");
    renderPlugins();
    expect(screen.getByRole("button", { name: /Phone/ })).toBeInTheDocument();
  });
});
