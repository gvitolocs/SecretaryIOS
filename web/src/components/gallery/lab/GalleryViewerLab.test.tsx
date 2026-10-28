import { fireEvent, screen } from "@solidjs/testing-library";
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../../../api/gallery";
import { setLocale } from "../../../i18n";
import { setImageCoreForTests } from "../../../lib/imageCore";
import { galleryItem } from "../../../test/galleryFixtures";
import { labJob } from "../../../test/labFixtures";
import { Route } from "@solidjs/router";
import { renderAt } from "../../../test/router";
import { GalleryViewer } from "../GalleryViewer";

vi.mock("../../../api/gallery", async (original) => ({
  ...(await original<typeof import("../../../api/gallery")>()),
  getDetails: vi.fn(),
  patchItem: vi.fn(),
  batch: vi.fn(),
  listAlbums: vi.fn(),
  createLabJob: vi.fn(),
}));
vi.mock("../../../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));
const { createLabJob, batch } = vi.mocked(api);

beforeAll(() => setImageCoreForTests(null));
beforeEach(() => {
  setLocale("en");
  createLabJob.mockReset();
  batch.mockReset();
  createLabJob.mockResolvedValue(labJob("lab_1"));
});

function open(items = [galleryItem("a"), galleryItem("b", { kind: "video", mime: "video/mp4" })], trash = false) {
  renderAt("/", () => <Route path="/" component={() => <GalleryViewer items={items} index={0} onIndex={vi.fn()} onClose={vi.fn()} onChanged={vi.fn()} onRemoved={vi.fn()} trash={trash} />} />);
}

describe("Edit with Lucina… in the viewer", () => {
  it("has a toolbar button that opens the sheet for the photo on screen", async () => {
    open();
    fireEvent.click(screen.getByRole("button", { name: "Edit with Lucina…" }));
    expect(screen.getByRole("dialog", { name: "Edit with Lucina" })).toBeInTheDocument();
    fireEvent.input(screen.getByLabelText("What should change?"), { target: { value: "make it snow" } });
    fireEvent.click(screen.getByRole("button", { name: "Send to the Lab" }));
    await screen.findByText(/Sent to the Lab/);
    expect(createLabJob).toHaveBeenCalledWith("a", { prompt: "make it snow", referenceItemId: null });
  });

  it("keeps the viewer's shortcuts out of the sheet, so Delete on a focused idea cannot open the delete dialog", () => {
    open();
    fireEvent.click(screen.getByRole("button", { name: "Edit with Lucina…" }));
    const idea = screen.getByRole("button", { name: "Make it a black and white photo" });
    idea.focus();
    fireEvent.keyDown(idea, { key: "Delete" });
    fireEvent.keyDown(idea, { key: "r" });
    expect(screen.queryByRole("dialog", { name: /Move to Trash|Delete/ })).toBeNull();
    expect(screen.getAllByRole("dialog")).toHaveLength(2); // the viewer and the sheet
  });

  it("is not offered for videos or in the Trash", () => {
    open([galleryItem("v", { kind: "video", mime: "video/mp4" })]);
    expect(screen.queryByRole("button", { name: "Edit with Lucina…" })).toBeNull();
  });

  it("is not offered in the Trash", () => {
    open([galleryItem("a")], true);
    expect(screen.queryByRole("button", { name: "Edit with Lucina…" })).toBeNull();
  });
});
