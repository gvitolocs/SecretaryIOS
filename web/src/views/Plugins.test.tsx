import { Route, Router } from "@solidjs/router";
import { render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import PluginsView, { PLUGINS } from "./Plugins";

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
    for (const name of ["Voice memos", "Films", "Notes", "Tasks", "Connections", "Pokémon", "Lucina.s computer", "Wardrobe", "Server health"]) {
      expect(screen.getByRole("button", { name: new RegExp(name) })).toBeInTheDocument();
    }
  });

  it("opens Films from its card and has no Media card", () => {
    renderPlugins();
    expect(screen.getByRole("button", { name: /Films.*Film and Dansk film/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^Media/ })).toBeNull();
  });

  it("has a Scan QR code card that opens /scan", () => {
    expect(PLUGINS.find((p) => p.id === "scan")?.href).toBe("/scan");
    renderPlugins();
    expect(screen.getByRole("button", { name: /Scan QR code/ })).toHaveAttribute("data-plugin", "scan");
  });

  it("has a Server health card that opens /server-health", () => {
    const server = PLUGINS.find((p) => p.id === "server");
    expect(server?.href).toBe("/server-health");
    renderPlugins();
    expect(screen.getByRole("button", { name: /Server health/ })).toHaveAttribute("data-plugin", "server");
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
