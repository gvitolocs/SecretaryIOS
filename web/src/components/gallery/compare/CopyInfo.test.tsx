import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../../../api/gallery";
import { galleryItem } from "../../../test/galleryFixtures";
import { itemRenditions, withLatest } from "../../../test/galleryParityFixtures";
import { CopyInfo } from "./CopyInfo";

vi.mock("../../../api/gallery", async (original) => ({
  ...(await original<typeof import("../../../api/gallery")>()),
  getRenditions: vi.fn(),
}));
vi.mock("../../../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));
// The compare view has its own tests; here it is a stand-in that shows it was opened.
vi.mock("./CompareView", () => ({
  CompareView: (p: { name: string; onClose: () => void }) => (
    <div role="dialog" aria-label="compare view">
      <span>{p.name}</span>
      <button type="button" onClick={p.onClose}>
        close compare
      </button>
    </div>
  ),
}));

const { getRenditions } = vi.mocked(api);

beforeEach(() => {
  vi.clearAllMocks();
  getRenditions.mockResolvedValue(withLatest(itemRenditions()));
});

const open = (item = galleryItem("a")) => render(() => <CopyInfo item={item} />);

describe("Server copy", () => {
  it("shows the format, size, dimensions, checksum and where the file is kept", async () => {
    open();
    const region = await screen.findByRole("region", { name: "Copies" });
    expect(getRenditions).toHaveBeenCalledWith("a");
    expect(screen.getByRole("tab", { name: "Server copy" })).toHaveAttribute("aria-selected", "true");
    expect(within(region).getByText("JPEG")).toBeInTheDocument();
    expect(within(region).getByText("image/jpeg")).toBeInTheDocument();
    expect(within(region).getByText("2.4 MB")).toBeInTheDocument();
    expect(within(region).getByText("4000 × 3000")).toBeInTheDocument();
    const hash = within(region).getByText("aaaaaaaaaa…aaaaaa");
    expect(hash).toHaveAttribute("title", "a".repeat(64));
    expect(within(region).getByText("In the archive")).toBeInTheDocument();
  });

  it("lists the other stored forms and the older versions", async () => {
    getRenditions.mockResolvedValue(
      withLatest(
        itemRenditions({
          versions: [{ version: 1, format: "jpeg", mime: "image/jpeg", bytes: 1, blake3: "c".repeat(64), width: null, height: null, url: "/v1/media/items/a/original?version=1", replacedAt: null, stored: "store", reachable: true }],
        }),
      ),
    );
    open();
    const summary = await screen.findByText("4 display copies");
    expect(screen.getByText("1 older version kept")).toBeInTheDocument();
    fireEvent.click(summary);
    const rows = screen.getAllByRole("listitem").map((li) => li.textContent);
    expect(rows).toEqual([
      "Grid tileJPEG XL · 8.8 KB · 256 × 256",
      "PreviewJPEG XL · 57 KB · 1440 × 1080",
      "ThumbnailJPEG · 38 KB · 480 × 360",
      "LargeJPEG · 135 KB · 1600 × 1200",
    ]);
  });

  it("says a file isn't reachable", async () => {
    const r = withLatest(itemRenditions());
    r.original.reachable = false;
    getRenditions.mockResolvedValue(r);
    open();
    expect(await screen.findByText("The file isn't reachable right now")).toBeInTheDocument();
  });

  it("shows nothing when the server can't be asked", async () => {
    getRenditions.mockRejectedValue(new Error("down"));
    open();
    await waitFor(() => expect(getRenditions).toHaveBeenCalled());
    expect(screen.queryByRole("region", { name: "Copies" })).toBeNull();
  });
});

describe("Device copy", () => {
  it("shows what the device reported and whether it is the server's file", async () => {
    open();
    fireEvent.click(await screen.findByRole("tab", { name: "Device copy" }));
    const region = screen.getByRole("region", { name: "Copies" });
    expect(screen.getByRole("tab", { name: "Device copy" })).toHaveAttribute("aria-selected", "true");
    expect(within(region).getByText("Test iPhone")).toBeInTheDocument();
    expect(within(region).getByText("IMG_0001.JPG")).toBeInTheDocument();
    expect(within(region).getByText("public.jpeg")).toBeInTheDocument();
    expect(within(region).getByText("2.4 MB")).toBeInTheDocument();
    expect(within(region).getByText("aaaaaaaaaa…aaaaaa")).toBeInTheDocument();
    expect(within(region).getByRole("status")).toHaveTextContent("Same file as the server copy: the checksums match.");
    expect(within(region).getByText(/can't be read from here/)).toBeInTheDocument();
    // Back to the server's.
    fireEvent.click(screen.getByRole("tab", { name: "Server copy" }));
    expect(within(region).queryByText("Test iPhone")).toBeNull();
  });

  it("flags a different checksum, and a missing one", async () => {
    const r = withLatest(itemRenditions());
    r.device!.latest = { ...r.device!.latest!, blake3: "b".repeat(64) };
    getRenditions.mockResolvedValue(r);
    open();
    fireEvent.click(await screen.findByRole("tab", { name: "Device copy" }));
    expect(screen.getByRole("status")).toHaveTextContent("Different from the server copy.");
  });

  it("says the app sent no checksum, and compares sizes", async () => {
    const r = withLatest(itemRenditions());
    r.device!.latest = { ...r.device!.latest!, blake3: null };
    getRenditions.mockResolvedValue(r);
    open();
    fireEvent.click(await screen.findByRole("tab", { name: "Device copy" }));
    expect(screen.getByText("Not reported")).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Same size as the server copy");
  });

  it("counts several uploads (an edit adds one)", async () => {
    const r = itemRenditions();
    r.device!.uploads = [r.device!.uploads[0]!, { ...r.device!.uploads[0]!, uploadedAt: "2026-10-28T00:00:00.000Z" }];
    getRenditions.mockResolvedValue(withLatest(r));
    open();
    fireEvent.click(await screen.findByRole("tab", { name: "Device copy" }));
    expect(screen.getByText("2 uploads (an edit adds one)")).toBeInTheDocument();
  });

  it("is off for a picture that didn't come from a device", async () => {
    getRenditions.mockResolvedValue(withLatest(itemRenditions({ device: null })));
    open();
    const tab = await screen.findByRole("tab", { name: "Device copy" });
    expect(tab).toBeDisabled();
    expect(tab).toHaveAttribute("title", "This picture didn't come from a device.");
  });
});

describe("Compare", () => {
  it("opens the compare view for the picture", async () => {
    open();
    fireEvent.click(await screen.findByRole("button", { name: "Compare" }));
    const dialog = screen.getByRole("dialog", { name: "compare view" });
    expect(within(dialog).getByText("a.jpg")).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "close compare" }));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("is off when fewer than two forms can be shown", async () => {
    getRenditions.mockResolvedValue(withLatest(itemRenditions({ renditions: [] })));
    open();
    expect(await screen.findByRole("button", { name: "Compare" })).toBeDisabled();
  });

  it("is not offered for a video", async () => {
    open(galleryItem("v", { kind: "video", mime: "video/mp4" }));
    await screen.findByRole("region", { name: "Copies" });
    expect(screen.queryByRole("button", { name: "Compare" })).toBeNull();
  });
});
