import { Route, Router } from "@solidjs/router";
import { render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { setShowAdult } from "../lib/adult";
import { resetInstagramBadges, setInstagramBadges } from "../state/instagramBadge";
import PluginsView, { PLUGINS, visiblePlugins } from "./Plugins";

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

  it("has a Keep card that opens /keep", () => {
    renderPlugins();
    expect(screen.getByRole("button", { name: /^Keep/ })).toBeInTheDocument();
    expect(PLUGINS.find((p) => p.id === "keep")?.href).toBe("/keep");
  });

  it("has an Agents card that opens /agents", () => {
    renderPlugins();
    expect(PLUGINS.find((p) => p.id === "agents")?.href).toBe("/agents");
    expect(screen.getByRole("button", { name: /^Agents/ })).toHaveAttribute("data-plugin", "agents");
  });

  it("has a Voice devices card that opens /voice", () => {
    renderPlugins();
    expect(PLUGINS.find((p) => p.id === "voice")?.href).toBe("/voice");
    expect(screen.getByRole("button", { name: /^Voice devices/ })).toHaveAttribute("data-plugin", "voice");
  });

  it("has an Instagram card that opens /instagram", () => {
    renderPlugins();
    expect(PLUGINS.find((p) => p.id === "instagram")?.href).toBe("/instagram");
    expect(PLUGINS.find((p) => p.id === "instagram")?.adult).toBeUndefined();
    expect(screen.getByRole("button", { name: /^Instagram/ })).toHaveAttribute("data-plugin", "instagram");
  });

  it("shows a red count on the Instagram card when watched accounts have news", () => {
    renderPlugins();
    expect(document.querySelector('[data-plugin="instagram"] .plugin-badge')).toBeNull();
    setInstagramBadges({ total: 3, accounts: { natgeo: 3 } });
    const badge = document.querySelector('[data-plugin="instagram"] .plugin-badge');
    expect(badge).toHaveTextContent("3");
    expect(badge).toHaveAttribute("aria-label", "3 new");
    resetInstagramBadges();
    expect(document.querySelector('[data-plugin="instagram"] .plugin-badge')).toBeNull();
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

  it("hides adult plugins when this device's switch is off", () => {
    PLUGINS.push({ id: "adult-test", icon: () => null, title: () => "Adult test", adult: true, href: "/adult-test" });
    try {
      setShowAdult(true);
      expect(visiblePlugins().some((p) => p.id === "adult-test")).toBe(true);
      setShowAdult(false);
      expect(visiblePlugins().some((p) => p.id === "adult-test")).toBe(false);
      expect(localStorage.getItem("secretary.showAdultPlugins")).toBe("0");
      renderPlugins();
      expect(screen.getByRole("checkbox", { name: /Show adult plugins/ })).not.toBeChecked();
    } finally {
      PLUGINS.pop();
      setShowAdult(true);
    }
  });
});
