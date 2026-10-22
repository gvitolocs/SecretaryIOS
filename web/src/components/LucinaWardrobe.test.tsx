import { createMemoryHistory, MemoryRouter, Route } from "@solidjs/router";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { receiveItem } from "../state/chat";
import { reloadStageFromStorage, setStageHidden } from "../state/stage";
import { closeWardrobe, resetWardrobeState, wardrobeOpen } from "../state/wardrobe";
import { stubFetch, type Call } from "../test/fetch";
import { fake, resetFakeStage } from "../test/stage";
import { OUTFITS, settings } from "../test/wardrobe";
import Shell from "../views/Shell";

// No WebGL in tests: the Lucina stage is a fake.
vi.mock("../lib/stageLoader", () => import("../test/stage"));

function serve(avatar = settings()): Call[] {
  let current = avatar;
  return stubFetch((c) => {
    if (c.url.startsWith("/v1/timeline")) return { body: { items: [], nextBefore: null } };
    if (c.url === "/v1/health") return { body: { ok: true, lucina: { reachable: true } } };
    if (c.url === "/v1/avatar/wardrobe") return { body: OUTFITS };
    if (c.url === "/v1/avatar" && c.method === "GET") return { body: current };
    if (c.url === "/v1/avatar" && c.method === "PUT") {
      current = { ...current, ...(c.body as object) };
      return { body: current };
    }
    return { status: 404 };
  });
}

function renderShell(path = "/") {
  const history = createMemoryHistory();
  history.set({ value: path, scroll: false, replace: true });
  render(() => (
    <MemoryRouter history={history} root={(p) => <Shell>{p.children}</Shell>}>
      <Route path="/" component={() => <p>Chat page</p>} />
      <Route path="/shopping" component={() => <p>Shopping page</p>} />
      <Route path="/gym" component={() => <p>Gym page</p>} />
    </MemoryRouter>
  ));
}

const go = (name: string) => fireEvent.click(screen.getAllByRole("link", { name })[0]!);

describe("Lucina's outfit in the shell", () => {
  beforeEach(() => {
    resetFakeStage();
    resetWardrobeState();
    localStorage.clear();
    reloadStageFromStorage();
  });
  afterEach(() => {
    closeWardrobe();
    resetWardrobeState();
  });

  it("starts in the section's outfit and changes with the route", async () => {
    serve();
    renderShell("/gym");
    await waitFor(() => expect(fake.stages).toHaveLength(1));
    const stage = fake.stages[0]!;
    expect(stage.options).toMatchObject({ outfit: "gym", assetBase: "/v1/assets/" });
    expect((stage.options as { wardrobe: unknown[] }).wardrobe).toHaveLength(4);

    go("Shopping");
    await waitFor(() => expect(stage.setOutfit).toHaveBeenLastCalledWith("chef"));
    go("Chat");
    await waitFor(() => expect(stage.setOutfit).toHaveBeenLastCalledWith("uniform"));
    expect(fake.stages).toHaveLength(1);
  });

  it("puts on what Lucina picked when the timeline says so", async () => {
    serve();
    renderShell("/");
    await waitFor(() => expect(fake.stages).toHaveLength(1));
    const stage = fake.stages[0]!;
    await waitFor(() => expect(stage.outfit).toBe("uniform"));
    receiveItem({ id: "x1", ts: Date.now(), channel: "telegram", role: "lucina", text: "Ecco!", meta: { kind: "avatar_outfit", outfit: "maid" } });
    await waitFor(() => expect(stage.setOutfit).toHaveBeenLastCalledWith("maid"));
  });

  it("opens the wardrobe from her controls, even with the stage hidden", async () => {
    serve();
    renderShell("/");
    await waitFor(() => expect(fake.stages).toHaveLength(1));
    fireEvent.click(screen.getByRole("button", { name: "Lucina's wardrobe" }));
    expect(wardrobeOpen()).toBe(true);
    await screen.findByRole("dialog", { name: "Wardrobe" });
    expect(document.querySelector("aside.dock")).toHaveClass("dock--wardrobe");
    closeWardrobe();

    setStageHidden(true);
    await waitFor(() => expect(document.querySelector("aside.dock")).toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "Account" }));
    fireEvent.click(await screen.findByRole("button", { name: /Lucina's wardrobe/ }));
    await waitFor(() => expect(document.querySelector("aside.dock")).not.toBeNull());
    expect(wardrobeOpen()).toBe(true);
    setStageHidden(false);
  });
});
