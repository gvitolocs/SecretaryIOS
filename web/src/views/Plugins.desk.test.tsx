import { Route } from "@solidjs/router";
import { fireEvent, screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { stubWide } from "../test/wide";
import { renderAt } from "../test/router";
import { setShowAdult } from "../lib/adult";
import { resetInstagramBadges, setInstagramBadges } from "../state/instagramBadge";
import Plugins, { PLUGINS, visiblePlugins } from "./Plugins";
beforeEach(() => { stubWide(); setShowAdult(false); });
afterEach(() => { vi.unstubAllGlobals(); resetInstagramBadges(); });
const routes = () => <><Route path="/plugins" component={Plugins} /><Route path="/films" component={() => <p>Films opened</p>} /></>;
it("renders the full grid, count, badges and working adult toggle", async () => {
  const { container } = renderAt("/plugins", routes);
  expect(screen.getByRole("heading", { level: 1, name: "Plugins" })).toHaveClass("pg-title");
  expect(screen.getByText(`${visiblePlugins().length} plugins`)).toHaveClass("pg-context");
  expect(container.querySelector(".pg-grid--full")).toBeInTheDocument();
  expect(container.querySelector(".plugins-page, .plugin-card, .pg-seg")).toBeNull();
  expect(container.querySelectorAll(".pg-cta").length).toBeLessThanOrEqual(1);
  expect(screen.getByRole("region", { name: "Plugins" }).querySelectorAll(".pg-card")).toHaveLength(visiblePlugins().length);
  expect(screen.queryByRole("button", { name: /^NClient/ })).toBeNull();
  fireEvent.click(screen.getByRole("checkbox"));
  expect(screen.getByRole("button", { name: /^NClient/ })).toBeInTheDocument();
  setInstagramBadges({ total: 3, accounts: { natgeo: 3 } });
  expect(screen.getByRole("img", { name: "3 new" })).toHaveTextContent("3");
  fireEvent.click(screen.getByRole("button", { name: /^Films/ }));
  expect(await screen.findByText("Films opened")).toBeInTheDocument();
});
it("has a friendly empty state if no plugins can run", () => {
  const saved = PLUGINS.splice(0);
  try {
    const { container } = renderAt("/plugins", routes);
    expect(screen.getByText("No plugins are available on this device.").closest(".pg-empty")).toBeInTheDocument();
    expect(container.querySelector(".pg-title")).toHaveTextContent("Plugins");
  } finally { PLUGINS.push(...saved); }
});
