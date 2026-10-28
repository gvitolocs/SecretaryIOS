import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../api/gallery";
import { renderAt } from "../test/router";
import { galleryShare } from "../test/galleryParityFixtures";
import GallerySharedLinksView from "./GallerySharedLinks";

vi.mock("../api/gallery", async (original) => ({
  ...(await original<typeof import("../api/gallery")>()),
  listShares: vi.fn(),
  revokeShare: vi.fn(),
  listAlbums: vi.fn(),
  createAlbumShare: vi.fn(),
}));

const { listShares, revokeShare, listAlbums, createAlbumShare } = vi.mocked(api);

const soon = () => new Date(Date.now() + 3 * 86_400_000 - 3_600_000).toISOString();

function open() {
  renderAt("/gallery/shared", () => (
    <>
      <Route path="/gallery/shared" component={GallerySharedLinksView} />
      <Route path="/gallery" component={() => <p>the gallery</p>} />
    </>
  ));
}

beforeEach(() => {
  vi.clearAllMocks();
  listAlbums.mockResolvedValue([{ id: "alb_1", title: "Trip", count: 5, system: false }]);
  revokeShare.mockResolvedValue(undefined);
  listShares.mockResolvedValue([
    galleryShare("LIVE1", { title: "Weekend", count: 3, views: 4, lastViewAt: "2026-10-27T10:00:00Z", expiresAt: soon(), allowOriginal: true }),
    galleryShare("ALBUM", { kind: "album", albumId: "alb_1", title: "Trip", count: 12, views: 0 }),
    galleryShare("OFF", { title: "Old one", live: false, revokedAt: "2026-10-01T00:00:00Z" }),
    galleryShare("GONE", { title: "Expired one", live: false, expiresAt: "2026-09-01T00:00:00Z" }),
  ]);
});

describe("the list", () => {
  it("shows both kinds, the live ones first, with views, expiry and the right to download", async () => {
    open();
    const live = await screen.findByRole("heading", { name: "Active · 2" });
    expect(live).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Turned off or expired · 2" })).toBeInTheDocument();
    const rows = screen.getAllByRole("listitem");
    expect(rows).toHaveLength(4);
    const weekend = rows.find((r) => within(r).queryByText("Weekend"))!;
    expect(within(weekend).getByText("Photos")).toBeInTheDocument();
    expect(within(weekend).getByText(/3 items/)).toBeInTheDocument();
    expect(within(weekend).getByText(/Opened 4 times/)).toBeInTheDocument();
    expect(within(weekend).getByText(/Works for 3 more days/)).toBeInTheDocument();
    expect(within(weekend).getByText(/Originals can be downloaded/)).toBeInTheDocument();
    const album = rows.find((r) => within(r).queryByText("Trip"))!;
    expect(within(album).getByText("Album")).toBeInTheDocument();
    expect(within(album).getByText(/Not opened yet/)).toBeInTheDocument();
    expect(within(album).getByText(/Works until you turn it off/)).toBeInTheDocument();
    // Links that are over have no actions.
    const off = rows.find((r) => within(r).queryByText("Old one"))!;
    expect(within(off).getByText(/Turned off/)).toBeInTheDocument();
    expect(within(off).queryByRole("button")).toBeNull();
    const gone = rows.find((r) => within(r).queryByText("Expired one"))!;
    expect(within(gone).getByText(/Expired Sep/)).toBeInTheDocument();
  });

  it("says when nothing has been shared", async () => {
    listShares.mockResolvedValue([]);
    open();
    expect(await screen.findByText(/haven't shared anything yet/)).toBeInTheDocument();
  });

  it("says when the list can't be loaded and tries again", async () => {
    listShares.mockRejectedValueOnce(new Error("down"));
    open();
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't load the shared links");
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("heading", { name: "Active · 2" })).toBeInTheDocument();
  });
});

describe("copying and opening", () => {
  it("copies the public address of a link and opens it in a new tab", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    open();
    const row = (await screen.findByText("Weekend")).closest("li")!;
    fireEvent.click(within(row).getByRole("button", { name: /Copy link/ }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("https://getsecretary.tech/a/LIVE1"));
    expect(await within(row).findByText("Copied")).toBeInTheDocument();
    const open_ = within(row).getByRole("link", { name: "Open" });
    expect(open_).toHaveAttribute("href", "https://getsecretary.tech/a/LIVE1");
    expect(open_).toHaveAttribute("target", "_blank");
    expect(open_).toHaveAttribute("rel", expect.stringContaining("noopener"));
    delete (navigator as unknown as Record<string, unknown>).clipboard;
  });
});

describe("turning a link off", () => {
  it("asks first, then revokes it and moves it to the ended ones", async () => {
    open();
    const row = (await screen.findByText("Weekend")).closest("li")!;
    fireEvent.click(within(row).getByRole("button", { name: /Turn off link/ }));
    expect(revokeShare).not.toHaveBeenCalled();
    expect(within(row).getByText("Turn it off for good?")).toBeInTheDocument();
    fireEvent.click(within(row).getByRole("button", { name: "Turn off" }));
    await waitFor(() => expect(revokeShare).toHaveBeenCalledWith("LIVE1"));
    expect(await screen.findByRole("heading", { name: "Active · 1" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Turned off or expired · 3" })).toBeInTheDocument();
  });

  it("can be cancelled and reports a refusal", async () => {
    revokeShare.mockRejectedValueOnce(new Error("no"));
    open();
    const row = (await screen.findByText("Weekend")).closest("li")!;
    fireEvent.click(within(row).getByRole("button", { name: /Turn off link/ }));
    fireEvent.click(within(row).getByRole("button", { name: "Cancel" }));
    expect(within(row).queryByText("Turn it off for good?")).toBeNull();
    fireEvent.click(within(row).getByRole("button", { name: /Turn off link/ }));
    fireEvent.click(within(row).getByRole("button", { name: "Turn off" }));
    expect(await within(row).findByRole("alert")).toHaveTextContent("That didn't work");
    expect(screen.getByRole("heading", { name: "Active · 2" })).toBeInTheDocument();
  });
});

describe("sharing an album", () => {
  it("picks an album and makes its link in the Share sheet", async () => {
    createAlbumShare.mockResolvedValue(galleryShare("NEWALB", { kind: "album", albumId: "alb_1", title: "Trip" }));
    open();
    await screen.findByRole("heading", { name: "Active · 2" });
    fireEvent.click(screen.getByRole("button", { name: /Share an album/ }));
    const picker = await screen.findByRole("dialog", { name: "Which album?" });
    fireEvent.click(within(picker).getByRole("button", { name: /Trip/ }));
    expect(await screen.findByRole("heading", { name: "Share album “Trip”" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Create link" }));
    await waitFor(() => expect(createAlbumShare).toHaveBeenCalledWith("alb_1", expect.objectContaining({ expiresInDays: 7 })));
    expect(await screen.findByLabelText("Share link")).toHaveValue("https://getsecretary.tech/a/NEWALB");
  });
});
