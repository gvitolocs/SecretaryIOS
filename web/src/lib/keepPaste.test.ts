import { describe, expect, it } from "vitest";
import { png, transfer } from "../test/clipboard";
import { dragHasFiles, imageFiles, pastedImages, pastedName } from "./keepPaste";

const at = () => new Date(2026, 9, 28, 22, 41, 5);
const names = (fs: File[]) => fs.map((f) => f.name);

describe("images in a paste or a drop", () => {
  it("takes a macOS screenshot (a nameless PNG, no text) and names it like a screenshot", () => {
    const p = pastedImages(transfer({ files: [png()], itemsOnly: true }), at);
    expect(names(p.files)).toEqual(["Image 2026-10-28 22.41.05.png"]);
    expect(p.files[0]!.type).toBe("image/png");
    expect(p.rejected).toBe(0);
  });

  it("takes a browser's 'Copy image' (bitmap plus an <img> in the HTML)", () => {
    const p = pastedImages(transfer({ files: [png()], html: '<meta charset="utf-8"><img src="https://x.test/cat.jpg" alt="">', text: "" }), at);
    expect(p.files).toHaveLength(1);
    // Firefox also puts the image URL in text/plain: the HTML still says it is only an image.
    expect(pastedImages(transfer({ files: [png()], html: "<img src=\"https://x.test/cat.jpg\">", text: "https://x.test/cat.jpg" }), at).files).toHaveLength(1);
  });

  it("takes several images at once, in order, with distinct names", () => {
    const p = pastedImages(transfer({ files: [png(), png("image.png", "image/jpeg"), png("Holiday.webp", "image/webp")] }), at);
    expect(names(p.files)).toEqual(["Image 2026-10-28 22.41.05.png", "Image 2026-10-28 22.41.05 (2).jpg", "Holiday.webp"]);
  });

  it("takes files copied in Finder (the text is just their names)", () => {
    const p = pastedImages(transfer({ files: [png("a.png"), png("b.png")], text: "a.png\nb.png" }), at);
    expect(names(p.files)).toEqual(["a.png", "b.png"]);
  });

  it("leaves a text paste alone, even with a picture of the text beside it (Excel, Word)", () => {
    expect(pastedImages(transfer({ text: "just text" })).files).toEqual([]);
    const excel = transfer({ files: [png()], text: "Oats\t3", html: "<table><tr><td>Oats</td><td>3</td></tr></table>" });
    expect(pastedImages(excel)).toEqual({ files: [], rejected: 0 });
  });

  it("counts images the server will not take instead of sending them", () => {
    const p = pastedImages(transfer({ files: [png("scan.tiff", "image/tiff"), png()] }), at);
    expect(p.files).toHaveLength(1);
    expect(p.rejected).toBe(1);
  });

  it("reads a drop's files when it has no items, and ignores non-images", () => {
    const pdf = new File(["%PDF"], "doc.pdf", { type: "application/pdf" });
    const dt = { files: [pdf, png("photo.png")], types: ["Files"], getData: () => "" } as unknown as DataTransfer;
    expect(names(imageFiles(dt).files)).toEqual(["photo.png"]);
    expect(imageFiles(null)).toEqual({ files: [], rejected: 0 });
  });

  it("tells a file drag from a note card being moved", () => {
    expect(dragHasFiles(transfer({ files: [png()] }))).toBe(true);
    expect(dragHasFiles(transfer({ text: "n_1" }))).toBe(false);
  });

  it("names JPEGs .jpg", () => {
    expect(pastedName("image/jpeg", at(), 2)).toBe("Image 2026-10-28 22.41.05 (3).jpg");
  });
});
