import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../../../i18n";
import { type Call, stubFetch } from "../../../test/fetch";
import { MemoriesStrip } from "./MemoriesStrip";

// The viewer has its own tests; here it shows what the strip hands it.
vi.mock("../GalleryViewer", () => ({
  GalleryViewer: (p: { items: { id: string }[]; index: number; onClose: () => void; onRemoved: (id: string) => void; onChanged: (i: unknown) => void }) => (
    <div data-testid="viewer" data-index={p.index}>
      <span>{p.items.map((i) => i.id).join(",")}</span>
      <button type="button" onClick={p.onClose}>
        close viewer
      </button>
      <button type="button" onClick={() => p.onRemoved(p.items[p.index]!.id)}>
        remove current
      </button>
      <button type="button" onClick={() => p.onChanged({ ...p.items[p.index]!, favorite: true })}>
        favorite current
      </button>
    </div>
  ),
}));
vi.mock("../../../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));

const raw = (id: string, extra: Record<string, unknown> = {}) => ({
  id,
  kind: "photo",
  title: id,
  mime: "image/jpeg",
  size: 1,
  taken_at: "2023-10-28T10:00:00Z",
  created_at: "2026-10-27T10:00:00Z",
  urls: { thumb: `/v1/media/items/${id}/thumb?size=256` },
  ...extra,
});

function backend(opts: { years?: unknown[]; recent?: unknown[]; fail?: boolean } = {}): Call[] {
  return stubFetch((call) => {
    const url = new URL(call.url, "http://x");
    if (opts.fail) return { status: 500, body: { error: { code: "boom", message: "boom" } } };
    if (url.pathname === "/v1/media/items/on-this-day") return { body: { years: opts.years ?? [] } };
    if (url.pathname === "/v1/media/items/recent") return { body: { items: opts.recent ?? [], total: (opts.recent ?? []).length } };
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
}

beforeEach(() => {
  setLocale("en");
  try {
    localStorage.clear();
  } catch {
    // ignore
  }
});

describe("MemoriesStrip", () => {
  it("shows nothing when there are no memories or the server fails", async () => {
    backend();
    const { container, unmount } = render(() => <MemoriesStrip origin="all" />);
    await waitFor(() => expect(globalThis.fetch).toHaveBeenCalledTimes(2));
    expect(container.querySelector(".g-memories")).toBeNull();
    unmount();
    backend({ fail: true });
    const again = render(() => <MemoriesStrip origin="all" />);
    await waitFor(() => expect(globalThis.fetch).toHaveBeenCalled());
    expect(again.container.querySelector(".g-memories")).toBeNull();
  });

  it("asks for the Gallery's source and lists On this day cards, then Recently added", async () => {
    const calls = backend({
      years: [
        { year: 2024, years_ago: 2, total: 3, items: [raw("a1"), raw("a2")] },
        { year: 2021, years_ago: 5, total: 1, items: [raw("b1")] },
      ],
      recent: [raw("r1"), raw("r2"), raw("r3")],
    });
    render(() => <MemoriesStrip origin="device:phone-1" />);
    const strip = await screen.findByRole("region", { name: "Memories" });
    const cards = within(strip).getAllByRole("button", { name: /Open/ });
    expect(cards.map((c) => c.getAttribute("aria-label"))).toEqual(["Open the memory from 2024", "Open the memory from 2021", "Open the recently added photos"]);
    expect(cards[0]).toHaveTextContent("On this day");
    expect(cards[0]).toHaveTextContent("2 years ago");
    expect(cards[2]).toHaveTextContent("Recently added");
    expect(cards[2]).toHaveTextContent("3 photos");
    expect(cards[0]!.querySelector("img")).toHaveAttribute("src", "/v1/media/items/a1/thumb?size=256");
    const asked = calls.map((c) => new URL(c.url, "http://x"));
    expect(asked.every((u) => u.searchParams.get("origin") === "device:phone-1")).toBe(true);
    expect(asked.find((u) => u.pathname.endsWith("/recent"))!.searchParams.get("days")).toBe("14");
  });

  it("opens the viewer on the card's pictures and follows removals and edits", async () => {
    backend({ years: [{ year: 2024, years_ago: 2, total: 2, items: [raw("a1"), raw("a2")] }] });
    const changed = vi.fn();
    render(() => <MemoriesStrip origin="all" onMutated={changed} />);
    fireEvent.click(await screen.findByRole("button", { name: "Open the memory from 2024" }));
    const viewer = screen.getByTestId("viewer");
    expect(viewer).toHaveTextContent("a1,a2");
    fireEvent.click(screen.getByRole("button", { name: "favorite current" }));
    expect(changed).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "remove current" }));
    expect(screen.getByTestId("viewer")).toHaveTextContent(/^a2/);
    expect(changed).toHaveBeenCalledTimes(2);
    // The last one removed closes the viewer.
    fireEvent.click(screen.getByRole("button", { name: "remove current" }));
    expect(screen.queryByTestId("viewer")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Open the memory from 2024" }));
    fireEvent.click(screen.getByRole("button", { name: "close viewer" }));
    expect(screen.queryByTestId("viewer")).toBeNull();
  });

  it("folds away and remembers it", async () => {
    backend({ recent: [raw("r1")] });
    const { unmount } = render(() => <MemoriesStrip origin="all" />);
    await screen.findByRole("button", { name: "Open the recently added photos" });
    fireEvent.click(screen.getByRole("button", { name: "Hide memories" }));
    expect(screen.queryByRole("button", { name: "Open the recently added photos" })).toBeNull();
    unmount();
    render(() => <MemoriesStrip origin="all" />);
    expect(await screen.findByRole("button", { name: "Show memories" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Open the recently added photos" })).toBeNull();
  });
});
