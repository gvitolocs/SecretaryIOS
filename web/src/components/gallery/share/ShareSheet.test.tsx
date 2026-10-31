import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../../../api/gallery";
import { galleryItem } from "../../../test/galleryFixtures";
import { galleryShare } from "../../../test/galleryParityFixtures";
import * as lib from "../../../lib/galleryShare";
import { ShareSheet, type ShareTarget } from "./ShareSheet";

vi.mock("../../../api/gallery", async (original) => ({
  ...(await original<typeof import("../../../api/gallery")>()),
  createSelectionShare: vi.fn(),
  createAlbumShare: vi.fn(),
  revokeShare: vi.fn(),
}));
vi.mock("../../../lib/galleryShare", async (original) => ({
  ...(await original<typeof import("../../../lib/galleryShare")>()),
  startDownload: vi.fn(),
}));

const { createSelectionShare, createAlbumShare, revokeShare } = vi.mocked(api);
const startDownload = vi.mocked(lib.startDownload);

const photo = (id: string, over = {}) => galleryItem(id, { size: 1000, ...over });

function open(target: ShareTarget = { kind: "items", items: [photo("a")] }) {
  const onClose = vi.fn();
  render(() => <ShareSheet target={target} onClose={onClose} />);
  return { onClose };
}

const setNav = (value: Partial<Navigator>) => {
  for (const [key, v] of Object.entries(value)) Object.defineProperty(navigator, key, { configurable: true, value: v });
};

beforeEach(() => {
  vi.clearAllMocks();
  for (const key of ["share", "canShare", "clipboard"]) delete (navigator as unknown as Record<string, unknown>)[key];
  createSelectionShare.mockResolvedValue(galleryShare("TOKEN1", { title: "Shared photos", expiresAt: new Date(Date.now() + 7 * 86_400_000 - 3_600_000).toISOString() }));
  createAlbumShare.mockResolvedValue(galleryShare("TOKEN2", { kind: "album", albumId: "alb_1", title: "Trip", expiresAt: null }));
  revokeShare.mockResolvedValue(undefined);
});

describe("the heading", () => {
  it("names one item, several, or the album", () => {
    open();
    expect(screen.getByRole("heading", { name: "Share this item" })).toBeInTheDocument();
  });

  it("counts many items and skips what can't be shared", () => {
    open({ kind: "items", items: [photo("a"), photo("b"), { ...photo("c"), kind: "file" as never }] });
    expect(screen.getByRole("heading", { name: "Share 2 items" })).toBeInTheDocument();
  });

  it("names the album", () => {
    open({ kind: "album", id: "alb_1", title: "Trip" });
    expect(screen.getByRole("heading", { name: "Share album “Trip”" })).toBeInTheDocument();
    // An album has no files to send or download.
    expect(screen.queryByRole("button", { name: /Download/ })).toBeNull();
  });
});

describe("making a link", () => {
  it("creates it with the week's expiry and no downloads of originals by default", async () => {
    open({ kind: "items", items: [photo("a"), photo("b")] });
    fireEvent.click(screen.getByRole("button", { name: "Create link" }));
    await waitFor(() => expect(createSelectionShare).toHaveBeenCalledTimes(1));
    expect(createSelectionShare).toHaveBeenCalledWith(["a", "b"], { expiresInDays: 7, allowOriginal: false, title: "" });
    const field = await screen.findByLabelText("Share link");
    // jsdom serves from http://localhost: the link points at the public host.
    expect(field).toHaveValue("https://getsecretary.tech/a/TOKEN1");
    expect(screen.getByText(/Works for 7 more days/)).toBeInTheDocument();
    expect(screen.getByText(/Originals can't be downloaded/)).toBeInTheDocument();
  });

  it("takes the expiry, the right to download originals and a name from the options", async () => {
    open({ kind: "items", items: [photo("a"), photo("b")] });
    fireEvent.change(screen.getByLabelText("The link works for"), { target: { value: "never" } });
    fireEvent.click(screen.getByLabelText(/Allow downloading the originals/));
    fireEvent.input(screen.getByLabelText("Name (optional)"), { target: { value: "Weekend" } });
    fireEvent.click(screen.getByRole("button", { name: "Create link" }));
    await waitFor(() => expect(createSelectionShare).toHaveBeenCalled());
    expect(createSelectionShare).toHaveBeenCalledWith(["a", "b"], { expiresInDays: null, allowOriginal: true, title: "Weekend" });
  });

  it("offers the choices 1, 7 and 30 days or never", () => {
    open();
    const options = within(screen.getByLabelText("The link works for")).getAllByRole("option");
    expect(options.map((o) => o.textContent)).toEqual(["1 day", "7 days", "30 days", "Until I turn it off"]);
    expect(screen.getByLabelText("The link works for")).toHaveValue("7");
  });

  it("makes an album link through the album's route", async () => {
    open({ kind: "album", id: "alb_1", title: "Trip" });
    expect(screen.queryByLabelText("Name (optional)")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Create link" }));
    await waitFor(() => expect(createAlbumShare).toHaveBeenCalledWith("alb_1", { expiresInDays: 7, allowOriginal: false, title: "" }));
    expect(createSelectionShare).not.toHaveBeenCalled();
    expect(await screen.findByText(/Works until you turn it off/)).toBeInTheDocument();
  });

  it("says so when the link can't be made, and lets you try again", async () => {
    createSelectionShare.mockRejectedValueOnce(new Error("down"));
    open();
    fireEvent.click(screen.getByRole("button", { name: "Create link" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't create the link");
    fireEvent.click(screen.getByRole("button", { name: "Create link" }));
    expect(await screen.findByLabelText("Share link")).toBeInTheDocument();
  });

  it("copies the link", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    setNav({ clipboard: { writeText } as unknown as Clipboard });
    open();
    fireEvent.click(screen.getByRole("button", { name: "Create link" }));
    await screen.findByLabelText("Share link");
    fireEvent.click(screen.getByRole("button", { name: "Copy link" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("https://getsecretary.tech/a/TOKEN1"));
    expect(await screen.findByText("Link copied.")).toBeInTheDocument();
  });

  it("turns the link off", async () => {
    open();
    fireEvent.click(screen.getByRole("button", { name: "Create link" }));
    await screen.findByLabelText("Share link");
    fireEvent.click(screen.getByRole("button", { name: "Turn off link" }));
    await waitFor(() => expect(revokeShare).toHaveBeenCalledWith("TOKEN1"));
    expect(await screen.findByText("The link is turned off.")).toBeInTheDocument();
    // Back to the options: a new link can be made.
    expect(screen.getByRole("button", { name: "Create link" })).toBeInTheDocument();
  });

  it("hands the link to the system share sheet when there is one", async () => {
    const share = vi.fn().mockResolvedValue(undefined);
    setNav({ share });
    open();
    fireEvent.click(screen.getByRole("button", { name: "Create link" }));
    await screen.findByLabelText("Share link");
    fireEvent.click(screen.getByRole("button", { name: "Share link…" }));
    await waitFor(() => expect(share).toHaveBeenCalledWith({ title: "Shared photos", url: "https://getsecretary.tech/a/TOKEN1" }));
  });
});

describe("sending the files", () => {
  const stubFiles = () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (href: string) => new Response(`bytes of ${href}`, { status: 200, headers: { "content-type": "image/jpeg" } })),
    );
  };

  it("shares the files themselves when the browser can", async () => {
    stubFiles();
    const share = vi.fn().mockResolvedValue(undefined);
    setNav({ share, canShare: vi.fn(() => true) });
    open({ kind: "items", items: [photo("a"), photo("b")] });
    fireEvent.click(screen.getByRole("button", { name: "Share the file…" }));
    await waitFor(() => expect(share).toHaveBeenCalledTimes(1));
    const arg = share.mock.calls[0]![0] as { files: File[] };
    expect(arg.files.map((f) => f.name)).toEqual(["IMG_a.JPG", "IMG_b.JPG"]);
    expect(arg.files[0]!.type).toBe("image/jpeg");
    expect(createSelectionShare).not.toHaveBeenCalled();
  });

  it("shares the link instead when the browser can't take files", async () => {
    stubFiles();
    const share = vi.fn().mockResolvedValue(undefined);
    setNav({ share, canShare: vi.fn(() => false) });
    open({ kind: "items", items: [photo("a")] });
    fireEvent.click(screen.getByRole("button", { name: "Share the file…" }));
    await waitFor(() => expect(share).toHaveBeenCalledWith({ title: "Shared photos", url: "https://getsecretary.tech/a/TOKEN1" }));
    expect(createSelectionShare).toHaveBeenCalledWith(["a"], { expiresInDays: 7, allowOriginal: false, title: "" });
    expect(await screen.findByText(/the link was shared/)).toBeInTheDocument();
  });

  it("shares the link when there are too many files to hold at once", async () => {
    const share = vi.fn().mockResolvedValue(undefined);
    const fetchSpy = vi.fn();
    vi.stubGlobal("fetch", fetchSpy);
    setNav({ share, canShare: vi.fn(() => true) });
    const many = Array.from({ length: 11 }, (_, i) => photo(`p${i}`));
    open({ kind: "items", items: many });
    fireEvent.click(screen.getByRole("button", { name: "Share the file…" }));
    await waitFor(() => expect(share).toHaveBeenCalledTimes(1));
    expect(fetchSpy).not.toHaveBeenCalled();
    expect((share.mock.calls[0]![0] as { url?: string }).url).toBe("https://getsecretary.tech/a/TOKEN1");
  });

  it("has no share button where the browser has no share sheet", () => {
    open();
    expect(screen.queryByRole("button", { name: "Share the file…" })).toBeNull();
  });
});

describe("downloading", () => {
  it("downloads one original at once", async () => {
    open();
    fireEvent.click(screen.getByRole("button", { name: "Download" }));
    await waitFor(() => expect(startDownload).toHaveBeenCalledTimes(1));
    expect(startDownload).toHaveBeenCalledWith({ href: "/v1/media/items/a/download", name: "IMG_a.JPG" });
  });

  it("downloads several one after another", async () => {
    open({ kind: "items", items: [photo("a"), photo("b"), photo("c")] });
    fireEvent.click(screen.getByRole("button", { name: "Download 3 originals" }));
    await waitFor(() => expect(startDownload).toHaveBeenCalledTimes(3), { timeout: 3000 });
    expect(startDownload.mock.calls.map((c) => c[0].name)).toEqual(["IMG_a.JPG", "IMG_b.JPG", "IMG_c.JPG"]);
  });
});

describe("closing", () => {
  it("closes on Escape and on Done", () => {
    const { onClose } = open();
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("links to the list of shared links", () => {
    open();
    expect(screen.getByRole("link", { name: "Shared links" })).toHaveAttribute("href", "/gallery/shared");
  });
});
