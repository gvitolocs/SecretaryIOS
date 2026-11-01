import { fireEvent, render, screen } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import DriveBar, { DEFAULT_PLACE, placeAt, readPlace, type DriveBarAction } from "./DriveBar";

const KEY = "test.bar";

function bar(over: Partial<Parameters<typeof DriveBar>[0]> = {}) {
  const calls = { take: 0, give: 0, exit: 0, interact: 0 };
  const props = {
    driving: false,
    status: "Watching",
    onTakeOver: () => calls.take++,
    onGiveBack: () => calls.give++,
    onExit: () => calls.exit++,
    onInteract: () => calls.interact++,
    storageKey: KEY,
    ...over,
  };
  render(() => <DriveBar {...props} />);
  return calls;
}

/** A pointer event jsdom can dispatch (it has no PointerEvent of its own). */
function pointer(type: string, x: number, y: number): Event {
  const e = new MouseEvent(type, { bubbles: true, cancelable: true, clientX: x, clientY: y, button: 0 });
  Object.defineProperty(e, "pointerId", { value: 1 });
  return e;
}

describe("DriveBar", () => {
  beforeEach(() => {
    setLocale("en");
    localStorage.clear();
  });
  afterEach(() => vi.restoreAllMocks());

  it("watching: the status and Take over; driving: Give back, the actions and the keys", () => {
    const calls = bar();
    expect(screen.getByRole("status")).toHaveTextContent("Watching");
    fireEvent.click(screen.getByRole("button", { name: "Take over" }));
    expect(calls.take).toBe(1);
    expect(calls.interact).toBe(1);
    expect(screen.queryByRole("button", { name: "Give back" })).toBeNull();
  });

  it("buttons act, then give the keyboard back to the stream, and never take focus", () => {
    const run = vi.fn();
    const actions: DriveBarAction[] = [{ id: "clipboard", label: "Clipboard sync on", icon: "📋", pressed: true, run }];
    const calls = bar({ driving: true, status: "You're driving", actions, panel: <p>keys here</p> });
    const sync = screen.getByRole("button", { name: "Clipboard sync on" });
    expect(sync).toHaveAttribute("aria-pressed", "true");
    const down = new MouseEvent("mousedown", { bubbles: true, cancelable: true });
    sync.dispatchEvent(down);
    expect(down.defaultPrevented).toBe(true);
    fireEvent.click(sync);
    expect(run).toHaveBeenCalledTimes(1);
    expect(calls.interact).toBe(1);
    fireEvent.click(screen.getByRole("button", { name: "Give back" }));
    expect(calls.give).toBe(1);
    fireEvent.click(screen.getByRole("button", { name: "Exit full screen" }));
    expect(calls.exit).toBe(1);
  });

  it("the keys popover opens and closes through its owner", () => {
    const [open, setOpen] = createSignal(false);
    render(() => (
      <DriveBar driving status="You're driving" onTakeOver={() => {}} onGiveBack={() => {}} onExit={() => {}} panel={<p>keys here</p>} panelOpen={open()} onPanel={setOpen} storageKey={KEY} />
    ));
    expect(screen.queryByText("keys here")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Keys" }));
    expect(screen.getByText("keys here")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Keys" }));
    expect(screen.queryByText("keys here")).toBeNull();
  });

  it("folds into a thin tab that opens on hover or click, and remembers it", () => {
    vi.useFakeTimers();
    try {
      bar({ driving: true, status: "You're driving" });
      fireEvent.click(screen.getByRole("button", { name: "Fold the toolbar away" }));
      expect(screen.queryByRole("button", { name: "Give back" })).toBeNull();
      expect(readPlace(KEY).folded).toBe(true);
      const root = document.querySelector(".drivebar")!;
      expect(root.getAttribute("data-open")).toBe("no");
      expect(screen.getByRole("button", { name: "You're driving — Show the toolbar" })).toBeInTheDocument();
      // Hovering peeks; leaving folds it again a moment later.
      fireEvent.mouseEnter(root);
      expect(screen.getByRole("button", { name: "Give back" })).toBeInTheDocument();
      fireEvent.mouseLeave(root);
      vi.advanceTimersByTime(800);
      expect(screen.queryByRole("button", { name: "Give back" })).toBeNull();
      // A click on the tab opens it too (touch screens have no hover); the pin keeps it open.
      fireEvent.click(screen.getByRole("button", { name: "You're driving — Show the toolbar" }));
      fireEvent.click(screen.getByRole("button", { name: "Keep the toolbar open" }));
      expect(readPlace(KEY).folded).toBe(false);
      fireEvent.mouseLeave(root);
      vi.advanceTimersByTime(800);
      expect(screen.getByRole("button", { name: "Give back" })).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("drags along the top edge, or down to the bottom one, and stays there", () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1000 });
    Object.defineProperty(window, "innerHeight", { configurable: true, value: 800 });
    bar({ driving: true, status: "You're driving" });
    const root = document.querySelector<HTMLElement>(".drivebar")!;
    const grip = document.querySelector(".drivebar-grip")!;
    expect(root.dataset.edge).toBe("top");
    expect(root.style.left).toBe("50%");
    grip.dispatchEvent(pointer("pointerdown", 500, 10));
    grip.dispatchEvent(pointer("pointermove", 300, 700));
    expect(root.dataset.dragging).toBe("yes");
    grip.dispatchEvent(pointer("pointerup", 300, 700));
    expect(root.dataset.dragging).toBe("no");
    expect(root.dataset.edge).toBe("bottom");
    expect(root.style.left).toBe("30%");
    expect(readPlace(KEY)).toEqual({ edge: "bottom", x: 0.3, folded: false });
  });

  it("places and stored places are sane", () => {
    expect(readPlace("nothing.here")).toEqual(DEFAULT_PLACE);
    localStorage.setItem(KEY, JSON.stringify({ edge: "left", x: 9, folded: "yes" }));
    expect(readPlace(KEY)).toEqual({ edge: "top", x: 0.95, folded: false });
    expect(placeAt(0, 100, 1000, 800, true)).toEqual({ edge: "top", x: 0.05, folded: true });
    expect(placeAt(990, 799, 1000, 800, false)).toEqual({ edge: "bottom", x: 0.95, folded: false });
  });
});
