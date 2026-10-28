import { Route } from "@solidjs/router";
import { screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { rawItem, sizeTheBox } from "../test/galleryGrid";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import GalleryView from "./Gallery";

vi.mock("../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));
vi.mock("../components/gallery/browse/MemoriesStrip", () => ({ MemoriesStrip: () => null }));

beforeEach(() => {
  setLocale("en");
  sizeTheBox();
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("Live Photos in the grid", () => {
  it("marks a Live Photo's tile with the LIVE badge and no other", async () => {
    stubFetch((call) => {
      const url = new URL(call.url, "http://x");
      if (url.pathname === "/v1/media/items") {
        return {
          body: {
            items: [
              rawItem("live1", { live_pair: "live1_mov" }, 0),
              rawItem("plain", {}, 1),
              rawItem("clip", { kind: "video", duration_ms: 4000, live_pair: "odd" }, 2),
            ],
            next_cursor: null,
          },
        };
      }
      return { body: { sources: [], all: { photos: 2, videos: 1 }, months: [] } };
    });
    renderAt("/gallery", () => <Route path="/gallery" component={() => <GalleryView />} />);
    await waitFor(() => expect(document.querySelectorAll(".g-tile")).toHaveLength(3));
    const badges = document.querySelectorAll(".g-badge--live");
    expect(badges).toHaveLength(1);
    expect(badges[0]!.closest(".g-tile")).toHaveAttribute("data-index", "0");
    // The tile says what it is to a screen reader.
    expect(screen.getByRole("button", { name: /^Live Photo/ })).toBeInTheDocument();
  });
});
