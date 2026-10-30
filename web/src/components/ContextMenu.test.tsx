import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ContextMenu, menuMode, placeMenu, type MenuAction, type MenuRequest } from "./ContextMenu";

const frame = () => new Promise((r) => setTimeout(r, 20));

function setup(actions: MenuAction[], request: MenuRequest = { x: 100, y: 100, mode: "popover" }) {
  const [open, setOpen] = createSignal<MenuRequest | null>(request);
  const onClose = vi.fn(() => setOpen(null));
  render(() => (
    <>
      <button type="button">opener</button>
      <ContextMenu request={open()} actions={actions} label="Photo actions" onClose={onClose} />
    </>
  ));
  return { onClose, setOpen };
}

const acts = (runs: Record<string, () => void> = {}): MenuAction[] => [
  { id: "a", label: "Alpha", run: runs.a ?? (() => {}) },
  { id: "b", label: "Beta", disabled: true, run: () => {} },
  { id: "c", label: "Gamma", run: runs.c ?? (() => {}) },
  { id: "d", label: "Delete", danger: true, separator: true, run: () => {} },
];

describe("ContextMenu", () => {
  afterEach(() => cleanup());

  it("opens right of and below the pointer, flipped or clamped near the window's edges", () => {
    expect(placeMenu(100, 100, 200, 300, 1000, 800)).toEqual({ left: 100, top: 100 });
    // Too close to the right and bottom: flipped to the other side of the pointer.
    expect(placeMenu(900, 700, 200, 300, 1000, 800)).toEqual({ left: 700, top: 400 });
    // Bigger than the room either way: kept 8 px inside.
    expect(placeMenu(50, 50, 200, 900, 230, 800)).toEqual({ left: 8, top: 8 });
  });

  it("is a sheet for a touch or a narrow window, a popover for a mouse or a key", () => {
    expect(menuMode("touch")).toBe("sheet");
    vi.stubGlobal("innerWidth", 1200);
    expect(menuMode("mouse")).toBe("popover");
    expect(menuMode("keyboard")).toBe("popover");
    vi.stubGlobal("innerWidth", 390);
    expect(menuMode("mouse")).toBe("sheet");
    vi.unstubAllGlobals();
  });

  it("moves with the arrow keys (wrapping, skipping disabled actions), runs with Enter and closes", async () => {
    const c = vi.fn();
    const { onClose } = setup(acts({ c }));
    await frame();
    const menu = screen.getByRole("menu", { name: "Photo actions" });
    expect(document.activeElement).toHaveTextContent("Alpha");
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(document.activeElement).toHaveTextContent("Gamma");
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(document.activeElement).toHaveTextContent("Delete");
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(document.activeElement).toHaveTextContent("Alpha");
    fireEvent.keyDown(menu, { key: "ArrowUp" });
    fireEvent.keyDown(menu, { key: "ArrowUp" });
    expect(document.activeElement).toHaveTextContent("Gamma");
    fireEvent.keyDown(menu, { key: "Enter" });
    expect(onClose).toHaveBeenCalled();
    expect(c).toHaveBeenCalledOnce();
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("closes with Esc and gives focus back", async () => {
    const opener = () => screen.getByRole("button", { name: "opener" });
    const [open, setOpen] = createSignal<MenuRequest | null>(null);
    render(() => (
      <>
        <button type="button">opener</button>
        <ContextMenu request={open()} actions={acts()} label="Photo actions" onClose={() => setOpen(null)} />
      </>
    ));
    opener().focus();
    setOpen({ x: 10, y: 10, mode: "popover" });
    await frame();
    expect(document.activeElement).toHaveTextContent("Alpha");
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(opener());
  });

  it("closes on a click outside, on scroll and when the window loses focus; not on its own clicks", async () => {
    const { onClose, setOpen } = setup(acts());
    await frame();
    fireEvent.pointerDown(screen.getByRole("menuitem", { name: "Alpha" }));
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.pointerDown(document.body);
    expect(onClose).toHaveBeenCalledTimes(1);
    setOpen({ x: 1, y: 1, mode: "popover" });
    await frame();
    fireEvent.scroll(document.body);
    expect(onClose).toHaveBeenCalledTimes(2);
    setOpen({ x: 1, y: 1, mode: "popover" });
    await frame();
    fireEvent.blur(window);
    expect(onClose).toHaveBeenCalledTimes(3);
  });

  it("does nothing for a disabled action", async () => {
    setup(acts());
    await frame();
    const beta = screen.getByRole("menuitem", { name: "Beta" });
    expect(beta).toBeDisabled();
    fireEvent.click(beta);
    expect(screen.getByRole("menu")).toBeInTheDocument();
  });

  it("as a sheet: a title line, the actions and a Cancel row; the backdrop closes it", async () => {
    const { onClose } = setup(acts(), { x: 0, y: 0, mode: "sheet", title: "IMG_0001.jpg" });
    await frame();
    const sheet = screen.getByRole("menu", { name: "Photo actions" });
    expect(sheet).toHaveTextContent("IMG_0001.jpg");
    expect(screen.getAllByRole("menuitem")).toHaveLength(4);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
