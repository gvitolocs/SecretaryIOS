import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../../../api/gallery";
import { ApiError } from "../../../api/client";
import { setLocale } from "../../../i18n";
import { galleryItem } from "../../../test/galleryFixtures";
import { labJob } from "../../../test/labFixtures";
import { Route } from "@solidjs/router";
import { renderAt } from "../../../test/router";
import { LabSheet } from "./LabSheet";

vi.mock("../../../api/gallery", async (original) => ({
  ...(await original<typeof import("../../../api/gallery")>()),
  createLabJob: vi.fn(),
  listGallery: vi.fn(),
}));
const { createLabJob, listGallery } = vi.mocked(api);

function open() {
  const onClose = vi.fn();
  const onQueued = vi.fn();
  renderAt("/", () => <Route path="/" component={() => <LabSheet item={galleryItem("itm_a")} onClose={onClose} onQueued={onQueued} />} />);
  return { onClose, onQueued };
}

const prompt = () => screen.getByLabelText("What should change?") as HTMLTextAreaElement;
const type = (text: string) => fireEvent.input(prompt(), { target: { value: text } });
const send = () => screen.getByRole("button", { name: "Send to the Lab" });

beforeEach(() => {
  setLocale("en");
  createLabJob.mockReset();
  listGallery.mockReset();
  createLabJob.mockResolvedValue(labJob("lab_1"));
  listGallery.mockResolvedValue({ items: [galleryItem("itm_a"), galleryItem("itm_r1"), galleryItem("itm_r2")], nextCursor: null });
});

describe("LabSheet", () => {
  it("opens as a dialog with the prompt focused and sending disabled until something is typed", () => {
    open();
    expect(screen.getByRole("dialog", { name: "Edit with Lucina" })).toBeInTheDocument();
    expect(prompt()).toHaveFocus();
    expect(send()).toBeDisabled();
    type("   ");
    expect(send()).toBeDisabled();
    type("add a hat");
    expect(send()).toBeEnabled();
    expect(screen.getByText(/^9 \/ 1.?000$/)).toBeInTheDocument();
  });

  it("fills the prompt from an idea, then appends the next one", () => {
    open();
    fireEvent.click(screen.getByRole("button", { name: "Make it a black and white photo" }));
    expect(prompt().value).toBe("Make it a black and white photo");
    fireEvent.click(screen.getByRole("button", { name: "Turn it into a watercolour painting" }));
    expect(prompt().value).toBe("Make it a black and white photo. Turn it into a watercolour painting");
  });

  it("queues the edit and then says so, with a way to the Lab", async () => {
    const { onQueued, onClose } = open();
    type("remove the car");
    fireEvent.click(send());
    await screen.findByText(/Sent to the Lab/);
    expect(createLabJob).toHaveBeenCalledWith("itm_a", { prompt: "remove the car", referenceItemId: null });
    expect(onQueued).toHaveBeenCalledWith(expect.objectContaining({ id: "lab_1" }));
    expect(screen.getByRole("link", { name: "Open the Lab" })).toHaveAttribute("href", "/gallery/lab");
    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(onClose).toHaveBeenCalled();
  });

  it("takes a reference photo from the Gallery, without offering the photo being edited", async () => {
    open();
    type("wear the hat from the second photo");
    fireEvent.click(screen.getByRole("button", { name: "Choose from the Gallery…" }));
    expect(screen.getByRole("dialog", { name: "Choose a reference photo" })).toBeInTheDocument();
    const grid = await screen.findByRole("list");
    expect(listGallery).toHaveBeenCalledWith({ kind: "photo", limit: 60 });
    expect(within(grid).getAllByRole("button")).toHaveLength(2);
    expect(within(grid).queryByRole("button", { name: "itm_a.jpg" })).toBeNull();
    fireEvent.click(within(grid).getByRole("button", { name: "itm_r2.jpg" }));

    // Back on the form with the prompt kept and the reference shown.
    expect(prompt().value).toBe("wear the hat from the second photo");
    expect(screen.getByAltText("itm_r2.jpg")).toBeInTheDocument();
    fireEvent.click(send());
    await waitFor(() => expect(createLabJob).toHaveBeenCalledWith("itm_a", { prompt: "wear the hat from the second photo", referenceItemId: "itm_r2" }));
  });

  it("removes a reference again", async () => {
    open();
    type("x");
    fireEvent.click(screen.getByRole("button", { name: "Choose from the Gallery…" }));
    fireEvent.click(await screen.findByRole("button", { name: "itm_r1.jpg" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove reference" }));
    expect(screen.getByRole("button", { name: "Choose from the Gallery…" })).toBeInTheDocument();
    fireEvent.click(send());
    await waitFor(() => expect(createLabJob).toHaveBeenCalledWith("itm_a", { prompt: "x", referenceItemId: null }));
  });

  it("pages the picker and says when the photos cannot load", async () => {
    listGallery.mockResolvedValueOnce({ items: [galleryItem("itm_r1")], nextCursor: "next" });
    listGallery.mockResolvedValueOnce({ items: [galleryItem("itm_r3")], nextCursor: null });
    open();
    fireEvent.click(screen.getByRole("button", { name: "Choose from the Gallery…" }));
    await screen.findByRole("button", { name: "itm_r1.jpg" });
    fireEvent.click(screen.getByRole("button", { name: "Show more" }));
    await screen.findByRole("button", { name: "itm_r3.jpg" });
    expect(listGallery).toHaveBeenLastCalledWith({ kind: "photo", limit: 60, cursor: "next" });
    expect(screen.queryByRole("button", { name: "Show more" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Back" }));

    listGallery.mockRejectedValueOnce(new Error("offline"));
    fireEvent.click(screen.getByRole("button", { name: "Choose from the Gallery…" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("The photos couldn't be loaded.");
  });

  it("keeps the form and says what went wrong when sending fails", async () => {
    open();
    type("x");
    createLabJob.mockRejectedValueOnce(new ApiError(429, "http_429", "busy"));
    fireEvent.click(send());
    expect(await screen.findByRole("alert")).toHaveTextContent("The Lab queue is full");
    createLabJob.mockRejectedValueOnce(new ApiError(500, "http_500", "boom"));
    fireEvent.click(send());
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("Couldn't send it to the Lab. Try again."));
    expect(prompt().value).toBe("x");
    expect(send()).toBeEnabled();
  });

  it("sends with Cmd+Enter and closes with Cancel and Escape", async () => {
    const { onClose } = open();
    type("x");
    fireEvent.keyDown(prompt(), { key: "Enter", metaKey: true });
    await waitFor(() => expect(createLabJob).toHaveBeenCalledTimes(1));
    await screen.findByText(/Sent to the Lab/);
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("speaks Italian and Danish", () => {
    setLocale("it");
    open();
    expect(screen.getByRole("dialog", { name: "Modifica con Lucina" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Manda al Laboratorio" })).toBeInTheDocument();
  });
});
