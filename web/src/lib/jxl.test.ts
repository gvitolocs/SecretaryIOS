import { afterEach, describe, expect, it, vi } from "vitest";
import {
  chooseGridUrl,
  choosePreviewUrl,
  mediaOriginalUrl,
  mediaThumbUrl,
  resetJxlSupport,
  supportsJxl,
} from "./jxl";

type Behaviour = "load" | "error";

class FakeImage {
  static behaviour: Behaviour = "error";
  naturalWidth = 0;
  width = 0;
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  set src(_value: string) {
    setTimeout(() => {
      if (FakeImage.behaviour === "load") {
        this.naturalWidth = 2;
        this.width = 2;
        this.onload?.();
      } else {
        this.onerror?.();
      }
    }, 0);
  }
}

afterEach(() => {
  resetJxlSupport();
  vi.unstubAllGlobals();
});

describe("jxl", () => {
  it("uses the JXL grid only when supported, JPEG thumbs otherwise, and the item preview in the lightbox", () => {
    expect(chooseGridUrl("a/b", true)).toBe("/v1/media/items/a%2Fb/r/grid");
    expect(chooseGridUrl("a/b", false)).toBe("/v1/media/items/a%2Fb/thumb?size=256");
    expect(choosePreviewUrl("a/b", "/v1/media/items/a%2Fb/r/preview")).toBe("/v1/media/items/a%2Fb/r/preview");
    expect(choosePreviewUrl("a/b")).toBe("/v1/media/items/a%2Fb/thumb?size=1024");
    expect(mediaThumbUrl("x")).toBe("/v1/media/items/x/thumb?size=256");
    expect(mediaOriginalUrl("x")).toBe("/v1/media/items/x/original");
  });

  it("falls back when the tiny JXL image cannot decode", async () => {
    vi.stubGlobal("Image", FakeImage);
    FakeImage.behaviour = "error";
    await expect(supportsJxl()).resolves.toBe(false);
  });

  it("detects a decoder and caches the result", async () => {
    vi.stubGlobal("Image", FakeImage);
    FakeImage.behaviour = "load";
    await expect(supportsJxl()).resolves.toBe(true);
    FakeImage.behaviour = "error";
    await expect(supportsJxl()).resolves.toBe(true);
    resetJxlSupport();
    await expect(supportsJxl()).resolves.toBe(false);
  });
});
