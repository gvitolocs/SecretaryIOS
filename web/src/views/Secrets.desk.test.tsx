import { Route } from "@solidjs/router";
import { fireEvent, screen } from "@solidjs/testing-library";
import { afterEach, expect, it, vi } from "vitest";
import { closeSecretSheet, secretSheet } from "../state/secretSheet";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import { SecretsView } from "./Secrets";
afterEach(() => { closeSecretSheet(); vi.unstubAllGlobals(); });
function renderPage() { stubWide(); return renderAt("/secrets", () => <Route path="/secrets" component={SecretsView} />); }
it("shows names, locations and timestamps and opens Add", async () => {
  stubFetch(c => ({ body: c.url.includes("catalog") ? { projects: [{ id: "p", name: "Work", environments: [{ slug: "prod", name: "Production" }] }] } : { folders: [], secrets: [{ key: "API_KEY", updated_at: "2026-10-28T12:00:00Z" }] } }));
  const { container } = renderPage();
  expect(await screen.findByRole("heading", { level: 1, name: "Secrets" })).toBeInTheDocument();
  expect(await screen.findByText("API_KEY")).toBeInTheDocument();
  expect(screen.getByText("Work · prod · /")).toBeInTheDocument();
  expect(container.querySelector("time")).toHaveAttribute("datetime", "2026-10-28T12:00:00Z");
  expect(container.querySelectorAll(".pg-cta")).toHaveLength(1);
  expect(container.querySelector(".pg-card--list .pg-rows")).toBeInTheDocument();
  expect(container.querySelector(".picker-overlay, .gym-subnav, .pg-seg")).toBeNull();
  expect(secretSheet()).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Add secret" }));
  expect(secretSheet()).not.toBeNull();
  closeSecretSheet();
  expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Secrets");
});
it("keeps the header above an empty state", async () => {
  stubFetch(() => ({ body: { projects: [] } }));
  const { container } = renderPage();
  expect(await screen.findByText("No projects match.")).toBeInTheDocument();
  expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Secrets");
  expect(container.querySelector(".pg-empty")).toBeInTheDocument();
});
it("offers retry after a catalog failure", async () => {
  stubFetch(() => ({ status: 500, body: { error: "offline" } }));
  renderPage();
  expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't load your projects.");
  expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
});
