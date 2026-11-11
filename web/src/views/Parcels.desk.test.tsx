import { Route } from "@solidjs/router";
import { screen, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { stubFetch } from "../test/fetch";
import { daily, delivered, parcel } from "../test/parcelFixtures";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import ParcelsView from "./Parcels";

function serve() {
  const all = [parcel(), daily(), delivered()];
  return stubFetch((c) => {
    if (c.url === "/v1/parcels") return { body: { parcels: all, digestHour: 9 } };
    const m = /^\/v1\/parcels\/([^/]+)$/.exec(c.url);
    const found = all.find((p) => p.id === m?.[1]);
    return found ? { body: { parcel: found } } : { status: 404 };
  });
}

const show = (path: string) => renderAt(path, () => <Route path="/parcels/:id?" component={ParcelsView} />);

describe("desktop Parcels", () => {
  beforeEach(() => stubWide());
  afterEach(() => vi.unstubAllGlobals());

  it("uses the page header with one accent action and a split grid", async () => {
    serve();
    const { container } = show("/parcels");
    expect(await screen.findByRole("heading", { level: 1, name: "Parcels" })).toHaveClass("pg-title");
    expect(container.querySelectorAll(".pg-cta")).toHaveLength(1);
    expect(container.querySelector(".pg-grid--split")).toBeInTheDocument();
    expect(await screen.findByText("Pick a parcel to see its history.")).toBeInTheDocument();
    expect(await within(screen.getByRole("navigation", { name: "Tracked parcels" })).findAllByRole("listitem")).toHaveLength(3);
    expect(screen.getByText("Delivered and stopped (1)")).toBeInTheDocument();
  });

  it("shows the list and the open parcel side by side", async () => {
    serve();
    show("/parcels/pc-22222222");
    const detail = await screen.findByRole("region", { name: "RR123456789DK" });
    expect(within(detail).getByText("Not checked yet")).toBeInTheDocument();
    expect(within(detail).getByText("No events yet. They appear after the first successful check.")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "All parcels" })).toBeNull();
    expect(document.querySelector(".parcel-row--current")).toHaveAttribute("data-parcel", "pc-22222222");
  });
});
