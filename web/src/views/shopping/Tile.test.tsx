import { render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { Tile } from "./Tile";

// The owner's Casa list as the server sent it on 2026-10-28 (only the fields a tile uses). Secretary
// Code drew these four with emoji (its bundle had no item art); every one has an illustration.
const CASA = [
  { id: "de3776eb", name: "Uova", icon: "🥚", catalogId: "eggs", section: "dairy-eggs", iconPicked: false, art: "eggs" },
  { id: "f6cbf352", name: "Prosciutto crudo", icon: "🍖", catalogId: "prosciutto", section: "deli", iconPicked: false, art: "prosciutto" },
  { id: "0b0d7d02", name: "Prosciutto cotto", icon: "🍖", catalogId: "ham", section: "deli", iconPicked: false, art: "ham" },
] as const;

function tile(row: (typeof CASA)[number] | { id: string; name: string; icon: string; catalogId: string; section: string; iconPicked: boolean }) {
  const { container } = render(() => (
    <Tile
      flipId={row.id}
      kind="need"
      icon={row.icon}
      catalogId={row.catalogId}
      section={row.section}
      iconPicked={row.iconPicked}
      name={row.name}
      label={row.name}
      onTap={() => {}}
    />
  ));
  return container;
}

describe("Tile: the needed tiles of the Casa list", () => {
  for (const row of CASA) {
    it(`draws ${row.name} with its illustration, not ${row.icon}`, () => {
      const c = tile(row);
      const img = c.querySelector("img.item-art");
      expect(img?.getAttribute("src")).toMatch(new RegExp(`/${row.art}(-[\\w-]{8})?\\.svg(\\?|$)`));
      expect(c.querySelector(".item-emoji")).toBeNull();
      expect(c.textContent).not.toContain(row.icon);
    });
  }

  it("keeps an emoji the owner picked himself", () => {
    const c = tile({ id: "x", name: "Uova", icon: "🐣", catalogId: "eggs", section: "dairy-eggs", iconPicked: true });
    expect(c.querySelector("img.item-art")).toBeNull();
    expect(c.querySelector(".item-emoji")?.textContent).toBe("🐣");
  });
});
