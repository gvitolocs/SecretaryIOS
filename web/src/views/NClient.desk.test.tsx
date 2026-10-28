import { Route } from "@solidjs/router";
import { fireEvent, screen, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setShowAdult } from "../lib/adult";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import NClientView from "./NClient";

function serve() {
  return stubFetch((c) => {
    if (c.url === "/v1/nclient/status") return { body: { ok: true, data: { enabled: true, save: { gallery: true, library: false } } } };
    if (c.url.startsWith("/v1/nclient/search")) {
      return { body: { ok: true, data: { items: [{ id: 7, title: "Placeholder volume", pages: 6, language: "english", tags: [], thumbUrl: "/t", favorite: false, progress: null }], page: 1, numPages: 1 } } };
    }
    if (c.url === "/v1/nclient/favorites") {
      return { body: { ok: true, data: { items: [] } } };
    }
    return { status: 404 };
  });
}

describe("desktop NClient", () => {
  beforeEach(() => {
    stubWide();
    setShowAdult(true);
  });
  afterEach(() => vi.unstubAllGlobals());

  it("uses the page header, section nav and result grid", async () => {
    serve();
    renderAt("/nclient", () => <Route path="/nclient" component={NClientView} />);
    expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent("NClient");
    expect(await screen.findByText("Placeholder volume")).toBeInTheDocument();
    const nav = screen.getByRole("navigation", { name: "NClient" });
    expect(within(nav).getByRole("button", { name: "Search" })).toHaveAttribute("aria-current", "page");
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(0);
    expect(document.querySelector(".nclient-bar[aria-label]")).toBeNull();
    expect(document.querySelector(".nclient-desk-main")).toBeInTheDocument();
  });

  it("shows an empty favorites list under the same header", async () => {
    serve();
    renderAt("/nclient", () => <Route path="/nclient" component={NClientView} />);
    await screen.findByText("Placeholder volume");
    fireEvent.click(screen.getByRole("button", { name: "Favorites" }));
    expect(await screen.findByText("No galleries found")).toBeInTheDocument();
    const nav = screen.getByRole("navigation", { name: "NClient" });
    expect(within(nav).getByRole("button", { name: "Favorites" })).toHaveAttribute("aria-current", "page");
  });
});
