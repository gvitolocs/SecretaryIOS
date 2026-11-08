import { fireEvent, render, screen } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { CompareSlider } from "./CompareSlider";

function box() {
  return document.querySelector<HTMLElement>(".lab-compare")!;
}
const before = () => screen.getByAltText("Before") as HTMLImageElement;

describe("CompareSlider", () => {
  const view = () =>
    render(() => <CompareSlider before="/b.png" after="/a.png" beforeLabel="Before" afterLabel="After" label="Compare before and after" ratio={4 / 3} />);

  it("shows both pictures, split in the middle", () => {
    view();
    expect(before().getAttribute("src")).toBe("/b.png");
    expect(screen.getByAltText("After").getAttribute("src")).toBe("/a.png");
    expect(before().style.clipPath).toBe("inset(0 50% 0 0)");
    expect(parseFloat(box().style.aspectRatio)).toBeCloseTo(4 / 3);
    expect(screen.getByRole("slider", { name: "Compare before and after" })).toHaveValue("50");
  });

  it("moves with the keyboard control", () => {
    view();
    const slider = screen.getByRole("slider");
    fireEvent.input(slider, { target: { value: "20" } });
    expect(before().style.clipPath).toBe("inset(0 80% 0 0)");
    expect(slider).toHaveAttribute("aria-valuetext", "Before 20%");
    fireEvent.input(slider, { target: { value: "100" } });
    expect(before().style.clipPath).toBe("inset(0 0% 0 0)");
  });

  it("follows a pointer drag over the picture", () => {
    view();
    const el = box();
    el.getBoundingClientRect = () => ({ left: 100, width: 200, top: 0, right: 300, bottom: 100, height: 100, x: 100, y: 0, toJSON() {} }) as DOMRect;
    el.setPointerCapture = () => {};
    el.hasPointerCapture = () => true;
    fireEvent.pointerDown(el, { clientX: 150, button: 0, pointerId: 1 });
    expect(before().style.clipPath).toBe("inset(0 75% 0 0)");
    fireEvent.pointerMove(el, { clientX: 250, pointerId: 1 });
    expect(before().style.clipPath).toBe("inset(0 25% 0 0)");
    fireEvent.pointerMove(el, { clientX: 900, pointerId: 1 });
    expect(before().style.clipPath).toBe("inset(0 0% 0 0)");
  });

  it("falls back to 4:3 without a ratio", () => {
    render(() => <CompareSlider before="/b.png" after="/a.png" beforeLabel="Before" afterLabel="After" label="x" />);
    expect(box().style.aspectRatio).toBe("4 / 3");
  });
});
