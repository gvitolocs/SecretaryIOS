import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { Route } from "@solidjs/router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../api/gallery";
import { ApiError } from "../api/client";
import { setLocale } from "../i18n";
import { galleryItem } from "../test/galleryFixtures";
import { labJob } from "../test/labFixtures";
import { renderAt } from "../test/router";
import GalleryLab from "./GalleryLab";

vi.mock("../api/gallery", async (original) => ({
  ...(await original<typeof import("../api/gallery")>()),
  listLab: vi.fn(),
  acceptLabJob: vi.fn(),
  discardLabJob: vi.fn(),
  retryLabJob: vi.fn(),
  cancelLabJob: vi.fn(),
}));
const { listLab, acceptLabJob, discardLabJob, retryLabJob, cancelLabJob } = vi.mocked(api);

const done = labJob("lab_done", {
  status: "done",
  progress: 100,
  queuePosition: null,
  createdAt: "2026-10-28T08:00:00.000Z",
  urls: { before: "/v1/media/lab/lab_done/before", after: "/v1/media/lab/lab_done/result", reference: null },
});
const running = labJob("lab_run", { status: "running", phase: "generating", progress: 40, queuePosition: null, createdAt: "2026-10-28T09:00:00.000Z", prompt: "turn it into a watercolour" });
const queued = labJob("lab_q", { status: "queued", queuePosition: 2, createdAt: "2026-10-28T09:05:00.000Z", prompt: "remove the bin" });
const failed = labJob("lab_fail", { status: "failed", error: "the model runner failed (exit 3): out of memory", queuePosition: null, createdAt: "2026-10-27T09:00:00.000Z", prompt: "add snow" });

function view() {
  return renderAt("/gallery/lab", () => <Route path="/gallery/lab" component={GalleryLab} />);
}

const card = (id: string) => document.querySelector<HTMLElement>(`[data-job="${id}"]`)!;

beforeEach(() => {
  setLocale("en");
  for (const fn of [listLab, acceptLabJob, discardLabJob, retryLabJob, cancelLabJob]) fn.mockReset();
  listLab.mockResolvedValue({ enabled: true, jobs: [queued, failed, done, running] });
});

afterEach(() => vi.useRealTimers());

describe("GalleryLab", () => {
  it("lists the edits: the running one first, then the queue, the finished and the failed", async () => {
    view();
    expect(screen.getByRole("heading", { name: "AI Lab" })).toBeInTheDocument();
    await screen.findByText("turn it into a watercolour");
    const order = [...document.querySelectorAll<HTMLElement>("[data-job]")].map((c) => c.dataset.job);
    expect(order).toEqual(["lab_run", "lab_q", "lab_done", "lab_fail"]);
    expect(within(card("lab_run")).getByText("Editing… 40%")).toBeInTheDocument();
    expect(within(card("lab_run")).getByRole("progressbar")).toHaveAttribute("value", "40");
    expect(within(card("lab_q")).getByText("Number 2 in line")).toBeInTheDocument();
    expect(within(card("lab_fail")).getByRole("alert")).toHaveTextContent("out of memory");
    expect(screen.getByRole("link", { name: "Back to the Gallery" })).toHaveAttribute("href", "/gallery");
  });

  it("shows a finished edit as before and after with a slider", async () => {
    view();
    const slider = await within(await screen.findByText("remove the bin").then(() => card("lab_done"))).findByRole("slider", { name: "Compare before and after" });
    expect(slider).toBeInTheDocument();
    const imgs = card("lab_done").querySelectorAll("img");
    expect([...imgs].map((i) => i.getAttribute("src"))).toEqual(["/v1/media/lab/lab_done/result", "/v1/media/lab/lab_done/before"]);
  });

  it("accepts: the card becomes a saved one and the original is promised untouched", async () => {
    acceptLabJob.mockResolvedValue({
      job: labJob("lab_done", { status: "accepted", acceptedItemId: "itm_new", queuePosition: null, urls: { before: null, after: "/v1/media/items/itm_new/thumb?size=1024&v=1", reference: null } }),
      item: galleryItem("itm_new"),
      duplicate: false,
    });
    view();
    await screen.findByText("remove the bin");
    fireEvent.click(within(card("lab_done")).getByRole("button", { name: "Accept" }));
    await within(card("lab_done")).findByText("Saved to your library");
    expect(acceptLabJob).toHaveBeenCalledWith("lab_done");
    expect(within(card("lab_done")).getByText("Saved as a new picture. The original is untouched.")).toBeInTheDocument();
    expect(within(card("lab_done")).getByRole("link", { name: "Open in the Gallery" })).toHaveAttribute("href", "/gallery");
    expect(within(card("lab_done")).queryByRole("button", { name: "Accept" })).toBeNull();
  });

  it("discards: the card goes away", async () => {
    discardLabJob.mockResolvedValue(labJob("lab_done", { status: "discarded" }));
    view();
    await screen.findByText("remove the bin");
    fireEvent.click(within(card("lab_done")).getByRole("button", { name: "Discard" }));
    await waitFor(() => expect(card("lab_done")).toBeNull());
    expect(discardLabJob).toHaveBeenCalledWith("lab_done");
  });

  it("retries a failed edit, and cancels a queued one", async () => {
    retryLabJob.mockResolvedValue(labJob("lab_fail", { status: "queued", queuePosition: 3, createdAt: "2026-10-28T10:00:00.000Z" }));
    cancelLabJob.mockResolvedValue();
    view();
    await screen.findByText("remove the bin");
    fireEvent.click(within(card("lab_fail")).getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(retryLabJob).toHaveBeenCalledWith("lab_fail"));
    fireEvent.click(within(card("lab_q")).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(card("lab_q")).toBeNull());
    expect(cancelLabJob).toHaveBeenCalledWith("lab_q");
  });

  it("says so, and reloads, when the job moved on without the page", async () => {
    acceptLabJob.mockRejectedValue(new ApiError(409, "http_409", "the job is discarded"));
    view();
    await screen.findByText("remove the bin");
    const calls = listLab.mock.calls.length;
    fireEvent.click(within(card("lab_done")).getByRole("button", { name: "Accept" }));
    expect(await within(card("lab_done")).findByRole("alert")).toHaveTextContent("This edit has changed in the meantime");
    await waitFor(() => expect(listLab.mock.calls.length).toBeGreaterThan(calls));
    // Any other failure is the generic message and leaves the buttons usable.
    acceptLabJob.mockRejectedValue(new ApiError(500, "http_500", "x"));
    fireEvent.click(within(card("lab_done")).getByRole("button", { name: "Accept" }));
    await waitFor(() => expect(within(card("lab_done")).getByRole("alert")).toHaveTextContent("Couldn't do that. Try again."));
    expect(within(card("lab_done")).getByRole("button", { name: "Accept" })).toBeEnabled();
  });

  it("asks again every few seconds while something is moving, and picks up the result", async () => {
    vi.useFakeTimers();
    listLab.mockResolvedValueOnce({ enabled: true, jobs: [running] });
    listLab.mockResolvedValue({ enabled: true, jobs: [{ ...done, id: "lab_run", prompt: running.prompt, urls: { before: "/b", after: "/a", reference: null } }] });
    view();
    await vi.advanceTimersByTimeAsync(0);
    expect(card("lab_run").dataset.status).toBe("running");
    expect(listLab).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(2600);
    expect(listLab).toHaveBeenCalledTimes(2);
    expect(card("lab_run").dataset.status).toBe("done");
    // Nothing is moving now: the next look is much later.
    await vi.advanceTimersByTimeAsync(5000);
    expect(listLab).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(16_000);
    expect(listLab).toHaveBeenCalledTimes(3);
  });

  it("explains an empty Lab, a Lab without a model, and a failed load", async () => {
    listLab.mockResolvedValue({ enabled: false, jobs: [] });
    const { unmount } = view();
    expect(await screen.findByText(/No edits yet/)).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("isn't connected on the server yet");
    unmount();

    listLab.mockRejectedValueOnce(new Error("offline"));
    view();
    expect(await screen.findByRole("alert")).toHaveTextContent("The Lab couldn't be loaded.");
    listLab.mockResolvedValue({ enabled: true, jobs: [queued] });
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByText("remove the bin");
  });
});
