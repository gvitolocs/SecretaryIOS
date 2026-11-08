import { render, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Masonry } from "./Masonry";

// jsdom has no layout: give the grid a width and each card a height from its data-h.
class FakeResizeObserver {
  constructor(private cb: () => void) {}
  observe() {
    queueMicrotask(this.cb);
  }
  unobserve() {}
  disconnect() {}
}

const sizes = [100, 300, 50, 60, 80, 40];

describe("Masonry", () => {
  beforeEach(() => {
    vi.stubGlobal("ResizeObserver", FakeResizeObserver);
    Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, get: () => 752 });
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
      configurable: true,
      get(this: HTMLElement) {
        return Number(this.firstElementChild?.getAttribute("data-h") ?? 0);
      },
    });
  });
  afterEach(() => {
    delete (HTMLElement.prototype as unknown as Record<string, unknown>).clientWidth;
    delete (HTMLElement.prototype as unknown as Record<string, unknown>).offsetHeight;
  });

  it("places the cards left to right into the shortest column, 240 px wide with 16 px gaps", async () => {
    const items = sizes.map((h, i) => ({ id: `c${i}`, h }));
    const { container } = render(() => (
      <Masonry items={items} list={false} heading="Others">
        {(it) => <div data-h={it.h}>{it.id}</div>}
      </Masonry>
    ));
    const cell = (id: string) => container.querySelector(`[data-id="${id}"]`) as HTMLElement;
    await waitFor(() => expect(container.querySelector(".kp-masonry--ready")).not.toBeNull());
    const at = (id: string) => cell(id).style.transform;
    // Row one: three columns at x = 0, 256, 512.
    expect(at("c0")).toBe("translate(0px, 0px)");
    expect(at("c1")).toBe("translate(256px, 0px)");
    expect(at("c2")).toBe("translate(512px, 0px)");
    // c3 goes under c2 (the shortest column); c4 under c0.
    expect(at("c3")).toBe("translate(512px, 66px)");
    expect(at("c4")).toBe("translate(0px, 116px)");
    expect(cell("c0").style.width).toBe("240px");
    expect(container.querySelector(".kp-masonry")?.getAttribute("data-columns")).toBe("3");
    expect(container.querySelector(".kp-section-head")?.textContent).toBe("Others");
  });

  it("uses one 600 px column in list mode", async () => {
    const items = [{ id: "a", h: 10 }, { id: "b", h: 20 }];
    const { container } = render(() => (
      <Masonry items={items} list={true}>
        {(it) => <div data-h={it.h}>{it.id}</div>}
      </Masonry>
    ));
    await waitFor(() => expect(container.querySelector(".kp-masonry--ready")).not.toBeNull());
    expect((container.querySelector('[data-id="b"]') as HTMLElement).style.transform).toBe("translate(0px, 26px)");
    expect((container.querySelector('[data-id="a"]') as HTMLElement).style.width).toBe("600px");
  });
});
