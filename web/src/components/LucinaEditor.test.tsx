import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { stubFetch, type Call } from "../test/fetch";
import { fake, resetFakeStage } from "../test/stage";
import { settings, WARDROBE } from "../test/wardrobe";
import { avatarSettings, closeEditor, editorOpen, loadAvatar, openEditor, resetWardrobeState } from "../state/wardrobe";

vi.mock("../lib/stageLoader", () => import("../test/stage"));

const { default: LucinaEditor } = await import("./LucinaEditor");

const ACCESSORIES = [
  { id: "beret", slot: "head", glb: "acc-beret.glb", thumbnail: "acc-beret.png", name: { en: "Beret", it: "Basco" }, hideTiara: true },
  { id: "ribbon", slot: "hair", glb: "acc-ribbon.glb", name: { en: "Ribbon" } },
];

type Saved = { id: string; name: string; look: Record<string, unknown>; updatedMs: number };

/** A server that keeps looks like secretary-server (saveLook / deleteLook / current). */
function serve(): Call[] {
  let current: Record<string, unknown> = settings({ looks: [], current: null });
  return stubFetch((c) => {
    if (c.url === "/v1/avatar/wardrobe") return { body: { ...WARDROBE, accessories: ACCESSORIES, parts: [] } };
    if (c.url === "/v1/avatar" && c.method === "GET") return { body: current };
    if (c.url === "/v1/avatar" && c.method === "PUT") {
      const p = c.body as { saveLook?: { id?: string; name: string; look: Record<string, unknown> }; current?: string | null; deleteLook?: string };
      let looks = [...(current.looks as Saved[])];
      if (p.deleteLook) looks = looks.filter((l) => l.id !== p.deleteLook);
      if (p.saveLook) {
        const id = p.saveLook.id ?? p.saveLook.name.toLowerCase().replace(/[^a-z0-9]+/g, "-");
        const entry = { id, name: p.saveLook.name, look: p.saveLook.look, updatedMs: 1 };
        looks = looks.some((l) => l.id === id) ? looks.map((l) => (l.id === id ? entry : l)) : [...looks, entry];
      }
      let cur = current.current as string | null;
      if (p.current !== undefined) cur = p.current;
      if (cur && !looks.some((l) => l.id === cur)) cur = null;
      current = { ...current, looks, current: cur };
      return { body: current };
    }
    return { status: 404 };
  });
}

async function show() {
  const calls = serve();
  await loadAvatar();
  openEditor();
  render(() => <LucinaEditor />);
  await screen.findByRole("dialog", { name: "Edit Lucina" });
  await waitFor(() => expect(fake.stages.length).toBe(1));
  await screen.findByRole("heading", { name: "Looks" });
  return calls;
}

const panel = () => within(screen.getByRole("tabpanel"));
const rail = (name: string) => within(screen.getByRole("tablist", { name: "Edit Lucina" })).getByRole("tab", { name });
const puts = (calls: Call[]) => calls.filter((c) => c.method === "PUT").map((c) => c.body);

describe("Edit Lucina", () => {
  beforeEach(() => {
    resetWardrobeState();
    resetFakeStage();
  });
  afterEach(() => {
    closeEditor();
    resetWardrobeState();
  });

  it("opens on its own stage with the current look, rail after Custom Cast", async () => {
    await show();
    const tabs = within(screen.getByRole("tablist", { name: "Edit Lucina" })).getAllByRole("tab");
    expect(tabs.map((t) => t.textContent)).toEqual(["Looks", "Body", "Face", "Eyes", "Skin & makeup", "Hair", "Accessories", "Outfit"]);
    const options = fake.stages[0]!.options as Record<string, unknown>;
    expect(options.framing).toBe("full");
    expect(options.muted).toBe(true);
    expect(typeof options.prepareModel).toBe("function");
    expect(within(screen.getByRole("tabpanel")).getByText("As designed")).toBeInTheDocument();
  });

  it("moves body sliders live, one undo step per gesture, and frames the face for face sliders", async () => {
    await show();
    const stage = fake.stages[0]!;
    fireEvent.click(rail("Body"));
    const height = screen.getByRole("slider", { name: /Height/ });
    fireEvent.input(height, { target: { value: "70" } });
    fireEvent.input(height, { target: { value: "80" } });
    fireEvent.change(height, { target: { value: "80" } });
    expect(height).toHaveValue("80");
    expect(screen.getByText("Unsaved changes", { exact: false })).toBeInTheDocument();
    // the editor scaled the (empty) model and asked the stage to re-frame
    expect(stage.refit).toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    expect(screen.getByRole("slider", { name: /Height/ })).toHaveValue("50");
    fireEvent.click(screen.getByRole("button", { name: "Redo" }));
    expect(screen.getByRole("slider", { name: /Height/ })).toHaveValue("80");
    fireEvent.click(rail("Face"));
    expect(stage.setView).toHaveBeenLastCalledWith(expect.objectContaining({ zoom: 1 }));
    expect(screen.getByRole("slider", { name: /Eye size/ })).toHaveValue("50");
  });

  it("puts accessories on by slot and hides the tiara switch state", async () => {
    await show();
    fireEvent.click(rail("Accessories"));
    expect(panel().getByRole("tab", { name: "Head" })).toHaveAttribute("aria-selected", "true");
    fireEvent.click(screen.getByRole("button", { name: /Beret/ }));
    expect(screen.getByRole("button", { name: /Beret/ })).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(panel().getByRole("tab", { name: "Hair" }));
    expect(screen.queryByRole("button", { name: /Beret/ })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /Ribbon/ }));
    fireEvent.click(panel().getByRole("tab", { name: "Head" }));
    expect(panel().getByRole("tab", { name: "Hair" }).className).toContain("wchip--set");
    fireEvent.click(screen.getByRole("button", { name: "None" }));
    expect(screen.getByRole("button", { name: /Beret/ })).toHaveAttribute("aria-pressed", "false");
  });

  it("saves a new look by name, wears it, and lists it", async () => {
    const calls = await show();
    fireEvent.click(rail("Eyes"));
    fireEvent.click(screen.getByRole("button", { name: "#3d6fd1" }));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    const dialog = await screen.findByRole("dialog", { name: "Look name" });
    fireEvent.input(within(dialog).getByRole("textbox"), { target: { value: "Serata" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(avatarSettings().current).toBe("serata"));
    const body = puts(calls);
    expect(body[0]).toMatchObject({ saveLook: { name: "Serata", look: { eyes: { iris: "#3d6fd1", highlight: true } } } });
    expect(body[1]).toEqual({ current: "serata" });
    expect(await screen.findByText("Saved. She is wearing it now.")).toBeInTheDocument();
    fireEvent.click(rail("Looks"));
    expect(panel().getByText("Serata")).toBeInTheDocument();
    expect(screen.queryByText("Unsaved changes", { exact: false })).toBeNull();
  });

  it("asks before throwing away unsaved changes", async () => {
    await show();
    fireEvent.click(screen.getAllByRole("button", { name: "Randomise" })[0]!);
    fireEvent.click(screen.getByRole("button", { name: "Close the editor" }));
    const ask = screen.getByRole("alertdialog", { name: "Discard your changes?" });
    fireEvent.click(within(ask).getByRole("button", { name: "Keep editing" }));
    expect(editorOpen()).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Close the editor" }));
    fireEvent.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "Discard" }));
    expect(editorOpen()).toBe(false);
  });

  it("shows a message instead of the stage without WebGL", async () => {
    serve();
    await loadAvatar();
    fake.failStage = true;
    openEditor();
    render(() => <LucinaEditor />);
    expect(await screen.findByText("The 3D editor needs WebGL, which this browser can't use.")).toBeInTheDocument();
  });
});
