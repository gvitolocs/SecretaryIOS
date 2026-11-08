import { Route } from "@solidjs/router";
import { screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import PokemonView from "./Pokemon";

function serve() {
  return stubFetch((c) => {
    if (c.url === "/v1/game/state") {
      return {
        body: {
          running: false,
          map_name: "pallet",
          badges: 1,
          objective: "Find Professor Oak",
          party: [{ name: "Pikachu", species: "Pikachu", level: 5, hp: 20, max_hp: 20 }],
          events: [],
        },
      };
    }
    return { status: 404 };
  });
}

describe("desktop Pokémon", () => {
  beforeEach(() => stubWide());
  afterEach(() => vi.unstubAllGlobals());

  it("uses the page header, play action and game card", async () => {
    serve();
    renderAt("/pokemon", () => <Route path="/pokemon" component={PokemonView} />);
    expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent("Pokémon");
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(1);
    expect(await screen.findByRole("button", { name: "Play" })).toBeInTheDocument();
    expect(document.querySelector(".pokemon-desk-game")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { level: 1, name: "Pokémon" })).toBeInTheDocument();
  });
});
